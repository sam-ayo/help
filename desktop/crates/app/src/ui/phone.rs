//! The remote control from a phone. The `remote` service connects to the relay and talks to
//! the paired phones (the PWA in `pwa/`). This module gives the events of the conversation to
//! the service, does the prompts, stops, and permission answers of the phones, and shows the
//! "Phone" card: the QR code for a new phone, and the list of paired phones.
//!
//! The relay connection stops while the Mac sleeps. While a phone is connected, `caffeinate`
//! stops the sleep when the user does not use the Mac. A sleep when the lid closes needs
//! `pmset disablesleep`, which the user can set in the card. See `set_lid`.
//!
//! A closed Mac that runs in a bag can become hot, and it can use all of its battery. Thus
//! `set_lid` also starts `BATTERY_WATCH`, which turns the setting off below `MIN_BATTERY`.

use std::process::{Child, Command};
use std::sync::Arc;

use acp_host::{Event, ToolStatus};
use gpui::{
    AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, h_flex,
    button::{Button, ButtonVariants as _},
    switch::Switch,
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;
use remote::message::{self as rm, Event as RemoteEvent};
use remote::service::{self, FromService, Service, State, ToService};

use super::{AppView, Chat, Item, Queued, TOOL_DETAIL};

/// The size of one module of the QR code, in pixels of the image.
const QR_MODULE: u32 = 8;
/// The size of the QR code on the screen.
const QR_SIZE: f32 = 240.;

/// The battery level, in percent, below which `BATTERY_WATCH` turns `disablesleep` off.
const MIN_BATTERY: u8 = 20;
/// The time between two checks of the battery, in seconds.
const BATTERY_CHECK: u32 = 60;
/// The name of the watcher process, for `battery_watch_runs`.
const BATTERY_WATCH_NAME: &str = "help-battery-watch";
/// A shell script that runs as root, because only root can change `disablesleep`. It turns the
/// setting off when the Mac uses its battery and the level is below `$2` percent. It stops when
/// the setting is off. `$1` is the time between two checks. The app cannot do this itself,
/// because nobody can type the password while the lid is closed. The script has no single
/// quotes, so `pmset_command` can put it in single quotes.
const BATTERY_WATCH: &str = r#"while pmset -g | grep -Eq "^ *SleepDisabled[[:space:]]+1"; do
  batt=$(pmset -g batt)
  case "$batt" in
    *"Battery Power"*)
      level=$(printf "%s\n" "$batt" | grep -Eo "[0-9]+%" | head -n 1 | tr -d %)
      if [ "${level:-100}" -lt "$2" ]; then pmset -a disablesleep 0; exit 0; fi
      ;;
  esac
  sleep "$1"
done"#;

/// The state of the remote control in the app.
#[derive(Default)]
pub(super) struct Phone {
    service: Option<Service>,
    pub(super) state: State,
    /// The "Phone" card shows.
    pub(super) open: bool,
    /// The QR code of the pairing link that shows.
    qr: Option<(String, Arc<RenderImage>)>,
    /// The last status that the phones got.
    status: Option<(String, bool, u32)>,
    /// `caffeinate`, while a phone is connected.
    awake: Option<Child>,
    /// The Mac stays on when the lid closes. See `set_lid`.
    lid: bool,
    /// The password prompt for `lid` shows.
    lid_busy: bool,
    /// `BATTERY_WATCH` runs. It stops when the Mac restarts, but `lid` stays on.
    battery_watch: bool,
}

impl Phone {
    fn send(&self, message: ToService) {
        if let Some(service) = &self.service {
            service.send(message);
        }
    }

    /// Stop the sleep when the user does not use the Mac, while `on`. `-w` stops `caffeinate`
    /// when the app stops.
    fn keep_awake(&mut self, on: bool) {
        match (on, &mut self.awake) {
            (true, None) => {
                self.awake = Command::new("/usr/bin/caffeinate")
                    .args(["-i", "-w", &std::process::id().to_string()])
                    .spawn()
                    .inspect_err(|e| tracing::warn!("cannot start caffeinate: {e}"))
                    .ok();
            }
            (false, Some(child)) => {
                let _ = child.kill();
                let _ = child.wait();
                self.awake = None;
            }
            _ => {}
        }
    }
}

impl AppView {
    /// Start the service, and do the messages of the phones.
    pub(super) fn start_remote(&mut self, cx: &mut Context<Self>) {
        let (service, mut rx) = service::start(&self.env.runtime, self.env.remote.clone());
        self.phone.service = Some(service);
        self.phone.lid = sleep_disabled();
        self.phone.battery_watch = battery_watch_runs();
        self.reset_remote();
        cx.spawn(async move |this, cx| {
            while let Some(message) = rx.recv().await {
                if this.update(cx, |this, cx| this.on_remote(message, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_remote(&mut self, message: FromService, cx: &mut Context<Self>) {
        match message {
            // A prompt from a phone goes the same way as a message from the composer.
            FromService::Prompt { text } => {
                if self.chat.busy || self.queue.iter().any(|q| q.editing) {
                    self.queue.push(Queued { text, attachments: Vec::new(), editing: false });
                } else {
                    let _ = self.deliver(text, Vec::new(), cx);
                }
                self.scroll_to_end(cx);
            }
            FromService::Cancel => self.cancel(cx),
            FromService::Permission { request, option } => {
                if self.chat.permission.as_ref().is_some_and(|p| p.request == request) {
                    self.answer_permission(option, cx);
                }
            }
            FromService::State(state) => {
                self.phone.qr = match (&state.pairing, self.phone.qr.take()) {
                    (Some(link), Some((old, image))) if *link == old => Some((old, image)),
                    (Some(link), _) => qr_image(link).map(|image| (link.clone(), image)),
                    (None, _) => None,
                };
                self.phone.keep_awake(!state.connected.is_empty());
                self.phone.state = state;
            }
        }
        cx.notify();
    }

    /// Give an event of the agent to the phones. Call it before `apply`, because `apply` takes
    /// the event.
    pub(super) fn forward(&self, event: &Event) {
        // A new permission request replaces the old one. See `apply`.
        if let (Event::Permission { .. }, Some(old)) = (event, &self.chat.permission) {
            self.phone.send(ToService::Event(RemoteEvent::PermissionClosed { request: old.request }));
        }
        if let Some(event) = remote_event(event) {
            self.phone.send(ToService::Event(event));
        }
    }

    /// Give a new permission request to the phones. Call it after `apply`.
    pub(super) fn forward_permission(&self) {
        if let Some(p) = &self.chat.permission {
            self.phone.send(ToService::Event(RemoteEvent::Permission {
                request: p.request,
                title: p.title.clone(),
                options: p
                    .options
                    .iter()
                    .map(|o| rm::PermissionOption { id: o.id.clone(), name: o.name.clone(), allow: o.allow })
                    .collect(),
            }));
        }
    }

    pub(super) fn forward_permission_closed(&self, request: u64) {
        self.phone.send(ToService::Event(RemoteEvent::PermissionClosed { request }));
    }

    pub(super) fn forward_user_text(&self, text: &str) {
        self.phone.send(ToService::Event(RemoteEvent::UserText { text: text.into() }));
    }

    /// Give all of the conversation to the phones again, for example after a change of thread.
    pub(super) fn reset_remote(&mut self) {
        self.phone.status = None;
        self.phone.send(ToService::Reset(conversation_events(&self.chat)));
    }

    /// Tell the phones if the agent works, and how many messages wait. Call it after each change.
    pub(super) fn sync_remote_status(&mut self) {
        let status = (self.chat.kind.label().to_string(), self.chat.busy, self.queue.len() as u32);
        if self.phone.status.as_ref() != Some(&status) {
            self.phone.send(ToService::Status { agent: status.0.clone(), busy: status.1, queued: status.2 });
            self.phone.status = Some(status);
        }
    }

    pub(super) fn open_phone(&mut self, cx: &mut Context<Self>) {
        self.phone.open = true;
        // The watcher stops when the battery is low, and when the Mac restarts.
        self.phone.lid = sleep_disabled();
        self.phone.battery_watch = battery_watch_runs();
        // A laptop with no phone shows the QR code at once.
        if self.phone.state.devices.is_empty() && self.phone.state.pairing.is_none() {
            self.phone.send(ToService::StartPairing);
        }
        cx.notify();
    }

    /// Set `pmset disablesleep`. The Mac then stays on when the lid closes, and the phones can
    /// connect. macOS asks for the password of an administrator. The setting stays after the
    /// app stops.
    fn set_lid(&mut self, on: bool, cx: &mut Context<Self>) {
        self.phone.lid_busy = true;
        cx.notify();
        let done = cx.background_spawn(async move { run_pmset(on) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = done.await {
                tracing::info!("cannot change the sleep setting: {e:#}");
            }
            let _ = this.update(cx, |this, cx| {
                this.phone.lid_busy = false;
                this.phone.lid = sleep_disabled();
                this.phone.battery_watch = battery_watch_runs();
                cx.notify();
            });
        })
        .detach();
    }

    fn close_phone(&mut self, cx: &mut Context<Self>) {
        self.phone.open = false;
        self.phone.send(ToService::StopPairing);
        cx.notify();
    }

    /// The "Phone" card, over the window.
    pub(super) fn render_phone(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.phone.open {
            return None;
        }
        let theme = cx.theme().clone();
        let state = &self.phone.state;
        let connection = match (&state.error, state.online) {
            (Some(error), _) => error.clone(),
            (None, true) => "Connected to the relay.".into(),
            (None, false) => "Connecting to the relay.".into(),
        };

        let pairing = match (&self.phone.qr, &state.pairing) {
            (Some((link, image)), _) => {
                let copy = link.clone();
                v_flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(QR_SIZE))
                            .rounded_lg()
                            .overflow_hidden()
                            .child(gpui::img(image.clone()).size_full()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .text_center()
                            .child("Scan this code with the camera of your phone. The code works for one phone, for 5 minutes."),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("phone-copy-link")
                                    .small()
                                    .outline()
                                    .icon(KitIcon::Copy)
                                    .label("Copy link")
                                    .on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))),
                            )
                            .child(
                                Button::new("phone-stop-pairing")
                                    .small()
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.phone.send(ToService::StopPairing);
                                        cx.notify();
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            (None, Some(_)) => div().text_sm().child("Cannot make the QR code.").into_any_element(),
            (None, None) => Button::new("phone-pair")
                .primary()
                .small()
                .icon(KitIcon::Plus)
                .label("Pair a phone")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.phone.send(ToService::StartPairing);
                    cx.notify();
                }))
                .into_any_element(),
        };

        let devices = state.devices.iter().enumerate().map(|(i, d)| {
            let id = d.id.clone();
            let connected = state.connected.contains(&d.id);
            let (label, at) = if d.last_seen > 0 { ("Last connected", d.last_seen) } else { ("Paired", d.paired_at) };
            let seen = chrono::DateTime::from_timestamp(at, 0)
                .map(|t| format!("{label} {}", t.with_timezone(&chrono::Local).format("%b %-d, %Y, %-I:%M %p")))
                .unwrap_or_default();
            h_flex()
                .gap_2p5()
                .py_1p5()
                .child(Icon::new(KitIcon::Smartphone).size(px(16.)).text_color(theme.muted_foreground))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_medium().truncate().child(d.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(if connected { theme.success } else { theme.muted_foreground })
                                .child(if connected { "Connected now".to_string() } else { seen }),
                        ),
                )
                .child(
                    Button::new(("phone-remove", i))
                        .small()
                        .ghost()
                        .icon(KitIcon::Trash)
                        .tooltip("Remove this phone")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.phone.send(ToService::Remove(id.clone()));
                            cx.notify();
                        })),
                )
        });

        let card = v_flex()
            .id("phone-card")
            .w(px(380.))
            .max_h_full()
            .overflow_y_scroll()
            .p_5()
            .gap_4()
            .rounded(px(16.))
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .shadow_lg()
            // A click in the card does not close it.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(KitIcon::Smartphone).size(px(18.)))
                    .child(div().flex_1().text_base().font_semibold().child("Phone"))
                    .child(
                        Button::new("phone-close")
                            .ghost()
                            .small()
                            .icon(KitIcon::X)
                            .tooltip("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close_phone(cx))),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Send messages to the agent from your phone, and see its answers. The messages have end-to-end encryption."),
            )
            .child(pairing)
            .when(!state.devices.is_empty(), |el| {
                el.child(
                    v_flex()
                        .child(div().text_xs().font_semibold().text_color(theme.muted_foreground).child("PAIRED PHONES"))
                        .children(devices),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        Switch::new("phone-lid")
                            .checked(self.phone.lid)
                            .disabled(self.phone.lid_busy)
                            .label("Keep the Mac on with the lid closed")
                            .on_click(cx.listener(|this, on: &bool, _, cx| this.set_lid(*on, cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("Your phone can then connect when the lid is closed. macOS asks for your password. The setting stays on after you quit the app. On battery power, the Mac turns the setting off below {MIN_BATTERY}%. Do not put the Mac in a bag while this setting is on, because the Mac can become hot.")),
                    )
                    .when(self.phone.lid && !self.phone.battery_watch, |el| {
                        el.child(div().text_xs().text_color(theme.warning).child(format!(
                            "The {MIN_BATTERY}% battery limit stopped, for example after a restart of the Mac. Turn the setting off and on again."
                        )))
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(if state.error.is_some() { theme.danger } else { theme.muted_foreground })
                    .child(SharedString::from(connection)),
            );

        Some(
            div()
                .id("phone-overlay")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .bg(gpui::black().opacity(0.35))
                .occlude()
                .on_click(cx.listener(|this, _, _, cx| this.close_phone(cx)))
                .child(card)
                .into_any_element(),
        )
    }
}

fn status(s: ToolStatus) -> rm::Status {
    match s {
        ToolStatus::Pending => rm::Status::Pending,
        ToolStatus::Running => rm::Status::Running,
        ToolStatus::Done => rm::Status::Done,
        ToolStatus::Failed => rm::Status::Failed,
    }
}

/// The start of a long tool output, as the tool card shows it.
fn short(text: &str) -> String {
    text.chars().take(TOOL_DETAIL).collect()
}

fn turn_reason(reason: &str) -> String {
    match reason {
        "EndTurn" => "end_turn".into(),
        other => other.into(),
    }
}

/// The event for the phones, without the large tool input and output. The phones do not get
/// `Permission` here. See `forward_permission`.
fn remote_event(event: &Event) -> Option<RemoteEvent> {
    Some(match event {
        Event::AgentText(text) => RemoteEvent::AgentText { text: text.clone() },
        Event::Thought(text) => RemoteEvent::Thought { text: text.clone() },
        Event::ToolCall { id, title, kind, status: s, parent, .. } => RemoteEvent::ToolCall {
            id: id.clone(),
            title: title.clone(),
            kind: kind.clone(),
            status: status(*s),
            parent: parent.clone(),
        },
        Event::ToolUpdate { id, title, status: s, detail, .. } => RemoteEvent::ToolUpdate {
            id: id.clone(),
            title: title.clone(),
            status: s.map(status),
            detail: detail.as_deref().map(short),
        },
        Event::Plan(entries) => RemoteEvent::Plan {
            entries: entries.iter().map(|(text, s)| rm::PlanEntry { text: text.clone(), status: status(*s) }).collect(),
        },
        Event::TurnEnded(reason) => RemoteEvent::TurnEnded { reason: turn_reason(reason) },
        Event::Error(message) => RemoteEvent::Error { message: message.clone() },
        Event::Closed => RemoteEvent::Error { message: "The agent process stopped.".into() },
        Event::Ready { .. } | Event::AgentFile(_) | Event::Permission { .. } => return None,
    })
}

/// The conversation as events, for a phone that connects.
fn conversation_events(chat: &Chat) -> Vec<RemoteEvent> {
    let mut events = Vec::new();
    for item in &chat.items {
        match item {
            Item::User { text, took, .. } => {
                events.push(RemoteEvent::UserText { text: text.clone() });
                if took.is_some() {
                    events.push(RemoteEvent::TurnEnded { reason: "end_turn".into() });
                }
            }
            Item::Agent(text) => events.push(RemoteEvent::AgentText { text: text.clone() }),
            Item::Thought(text) => events.push(RemoteEvent::Thought { text: text.clone() }),
            Item::Tool { id, title, kind, status: s, detail, parent, .. } => {
                events.push(RemoteEvent::ToolCall {
                    id: id.clone(),
                    title: title.clone(),
                    kind: kind.clone(),
                    status: status(*s),
                    parent: parent.clone(),
                });
                if let Some(detail) = detail {
                    events.push(RemoteEvent::ToolUpdate { id: id.clone(), title: None, status: None, detail: Some(short(detail)) });
                }
            }
            Item::Plan(entries) => events.push(RemoteEvent::Plan {
                entries: entries.iter().map(|(text, s)| rm::PlanEntry { text: text.clone(), status: status(*s) }).collect(),
            }),
            Item::Error(message) => events.push(RemoteEvent::Error { message: message.clone() }),
            Item::Notice(_) | Item::AgentFiles(_) | Item::Compacted => {}
        }
    }
    events
}

/// True if `pmset disablesleep` is on. `pmset -g` shows "SleepDisabled 1" only then.
fn sleep_disabled() -> bool {
    Command::new("/usr/bin/pmset").arg("-g").output().is_ok_and(|out| {
        String::from_utf8_lossy(&out.stdout).lines().any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some("SleepDisabled") && words.next() == Some("1")
        })
    })
}

/// True if `BATTERY_WATCH` runs.
fn battery_watch_runs() -> bool {
    Command::new("/usr/bin/pgrep").args(["-f", BATTERY_WATCH_NAME]).output().is_ok_and(|out| out.status.success())
}

/// The command that `run_pmset` runs as root. With `on`, it also starts `BATTERY_WATCH`.
fn pmset_command(on: bool) -> String {
    if !on {
        return "/usr/bin/pmset -a disablesleep 0".into();
    }
    // `do shell script` waits until the output closes, so the watcher has no output.
    format!(
        "/usr/bin/pmset -a disablesleep 1 && PATH=/usr/bin:/bin:/usr/sbin:/sbin /usr/bin/nohup /bin/sh -c '{BATTERY_WATCH}' {BATTERY_WATCH_NAME} {BATTERY_CHECK} {MIN_BATTERY} >/dev/null 2>&1 &"
    )
}

/// The AppleScript that runs `pmset_command` as root, with the password prompt of macOS.
fn pmset_applescript(on: bool) -> String {
    let command = pmset_command(on).replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{command}\" with administrator privileges")
}

/// Set `pmset disablesleep` with the password prompt of macOS.
fn run_pmset(on: bool) -> anyhow::Result<()> {
    let out = Command::new("/usr/bin/osascript").args(["-e", &pmset_applescript(on)]).output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// The QR code of `text`, black on white, with a white border of 4 modules.
fn qr_image(text: &str) -> Option<Arc<RenderImage>> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width() as u32;
    let colors = code.to_colors();
    let side = (width + 8) * QR_MODULE;
    let mut bytes = vec![255u8; (side * side * 4) as usize];
    for y in 0..width {
        for x in 0..width {
            if colors[(y * width + x) as usize] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..QR_MODULE {
                let row = (y + 4) * QR_MODULE + dy;
                let start = ((row * side + (x + 4) * QR_MODULE) * 4) as usize;
                for px in bytes[start..start + (QR_MODULE * 4) as usize].chunks_mut(4) {
                    // BGRA, as RenderImage keeps it.
                    px.copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
    }
    let buffer = image::RgbaImage::from_raw(side, side, bytes)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_code_has_a_white_border() {
        let image = qr_image("https://example.com/#pair=abc").expect("a QR code");
        let bytes = image.as_bytes(0).expect("one frame");
        assert_eq!(&bytes[..4], &[255, 255, 255, 255]);
        assert!(bytes.chunks(4).any(|p| p == [0, 0, 0, 255]));
    }

    /// Run `BATTERY_WATCH` with a fake `pmset`, which shows `batt` and keeps `disablesleep` in a
    /// file. A fake `sleep` writes "slept" in that file, so the watcher stops after one check.
    /// Returns the content of the file at the end.
    fn watch(name: &str, batt: &str) -> String {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("help-battery-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = |file: &str, body: &str| {
            let path = dir.join(file);
            std::fs::write(&path, format!("#!/bin/sh\ncd {}\n{body}\n", dir.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        std::fs::write(dir.join("batt"), batt).unwrap();
        std::fs::write(dir.join("state"), "1").unwrap();
        script(
            "pmset",
            r#"case "$*" in
  "-g batt") cat batt ;;
  "-g") printf " SleepDisabled\t\t%s\n" "$(cat state)" ;;
  "-a disablesleep "*) echo "$3" > state ;;
esac"#,
        );
        script("sleep", "echo slept > state");
        let status = Command::new("/bin/sh")
            .args(["-c", BATTERY_WATCH, BATTERY_WATCH_NAME, "1", &MIN_BATTERY.to_string()])
            .env("PATH", format!("{}:/usr/bin:/bin", dir.display()))
            .status()
            .unwrap();
        assert!(status.success());
        let state = std::fs::read_to_string(dir.join("state")).unwrap().trim().to_string();
        let _ = std::fs::remove_dir_all(&dir);
        state
    }

    #[test]
    fn the_battery_watch_turns_the_setting_off_only_on_a_low_battery() {
        let batt = |source: &str, level: u8| {
            format!("Now drawing from '{source}'\n -InternalBattery-0 (id=1)\t{level}%; discharging; present: true\n")
        };
        assert_eq!(watch("low", &batt("Battery Power", 19)), "0");
        assert_eq!(watch("high", &batt("Battery Power", 66)), "slept");
        assert_eq!(watch("ac", &batt("AC Power", 5)), "slept");
        assert_eq!(watch("desktop", "Now drawing from 'AC Power'\n"), "slept");
    }

    /// The script and the command are correct, but they do not run.
    #[test]
    fn the_pmset_script_has_a_correct_syntax() {
        assert!(!BATTERY_WATCH.contains('\''), "pmset_command puts the watcher in single quotes");
        for on in [true, false] {
            let shell = Command::new("/bin/sh").args(["-n", "-c", &pmset_command(on)]).status().unwrap();
            assert!(shell.success());
            let out = std::env::temp_dir().join(format!("help-pmset-{}-{on}.scpt", std::process::id()));
            let script =
                Command::new("/usr/bin/osacompile").arg("-e").arg(pmset_applescript(on)).arg("-o").arg(&out).status();
            assert!(script.unwrap().success());
            let _ = std::fs::remove_file(out);
        }
    }

    #[test]
    fn a_turn_end_has_the_name_that_the_pwa_knows() {
        let event = remote_event(&Event::TurnEnded("EndTurn".into()));
        assert_eq!(event, Some(RemoteEvent::TurnEnded { reason: "end_turn".into() }));
    }
}
