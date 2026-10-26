//! The steps of a turn in a panel at the right side of the window: the thoughts, the tool
//! calls, the plans, and the text of the agent before the final reply. The chat does not show
//! the steps, so the reply stays near the message of the user. See `turn_parts`.

use acp_host::ToolStatus;
use gpui::{
    AppContext as _, Entity, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex,
    button::{Button, ButtonVariants as _},
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    text::TextView,
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;

use super::{AppView, Context, Item, LONG_TITLE, TOOL_DETAIL, TOOLBAR, accent, attach, message_style, plural, tool_icon, turn_parts};

/// The size of the number of a phase.
const BADGE: f32 = 18.;
/// The space at the left of the summary and the steps of a phase, below the text of the header.
const INDENT: f32 = BADGE + 8.;

/// The width of the panel when the app starts.
pub(super) const WIDTH: f32 = 420.;

/// The steps of one turn in the panel.
pub(super) struct Steps {
    /// The index of the user message of the turn.
    pub(super) turn: usize,
    /// The index of the newest user message when the panel changed last. When the user sends
    /// a new message, the panel shows the steps of the new turn.
    newest: usize,
    /// The items of the steps, the first step first. The panel has one row for each.
    items: Vec<usize>,
    scroller: Entity<MessageScrollerState>,
}

impl AppView {
    /// Show the steps of the turn that starts at the user message `turn` in the panel, or close
    /// the panel if it shows them already. The panel shows instead of a document or an app.
    pub(super) fn toggle_steps(&mut self, turn: usize, cx: &mut Context<Self>) {
        if self.steps.as_ref().is_some_and(|s| s.turn == turn) {
            self.close_steps(cx);
            return;
        }
        self.close_doc(cx);
        self.close_app(cx);
        let items = turn_parts(&self.chat, turn).steps;
        let count = items.len();
        self.steps = Some(Steps {
            turn,
            newest: self.newest_turn(),
            items,
            scroller: cx.new(|cx| MessageScrollerState::new(count, cx)),
        });
        self.rows_changed(cx);
        cx.notify();
    }

    /// Close the steps panel.
    pub(super) fn close_steps(&mut self, cx: &mut Context<Self>) {
        if self.steps.take().is_some() {
            self.rows_changed(cx);
            cx.notify();
        }
    }

    /// The index of the user message of the newest turn.
    fn newest_turn(&self) -> usize {
        self.chat.items.iter().rposition(|i| matches!(i, Item::User { .. })).unwrap_or(0)
    }

    /// Make the rows of the panel again after a change to the conversation, and measure again
    /// the rows from the row of the item `item`. A new step gets a new row at the end.
    pub(super) fn steps_changed(&mut self, item: usize, cx: &mut Context<Self>) {
        let newest = self.newest_turn();
        let Some(steps) = &mut self.steps else { return };
        // The user sent a new message. The panel follows the work of the agent on it.
        if steps.newest != newest {
            steps.newest = newest;
            steps.turn = newest;
            steps.items = turn_parts(&self.chat, newest).steps;
            let count = steps.items.len();
            steps.scroller.update(cx, |s, cx| s.reset(count, cx));
            return;
        }
        let items = turn_parts(&self.chat, steps.turn).steps;
        let old = std::mem::replace(&mut steps.items, items);
        let items = &steps.items;
        let same = old.iter().zip(items).take_while(|(a, b)| a == b).count();
        let from = items.iter().position(|&i| i >= item).unwrap_or(items.len()).min(same);
        steps.scroller.update(cx, |s, cx| {
            s.splice(same..old.len(), items.len() - same, cx);
            if from < items.len() {
                s.remeasure_items(from..items.len(), cx);
            }
        });
    }

    /// Measure again the row of the item `ix` in the panel, for example after the user opens
    /// the output of a tool.
    pub(super) fn remeasure_step(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(steps) = &self.steps else { return };
        if let Some(row) = steps.items.iter().position(|&i| i == ix) {
            steps.scroller.update(cx, |s, cx| {
                s.remeasure_items(row..row + 1, cx);
            });
        }
    }

    /// The panel with the steps of a turn. While the agent works on the turn, each new step
    /// shows at the end, and the panel follows it.
    pub(super) fn render_steps(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let steps = self.steps.as_ref()?;
        let theme = cx.theme().clone();
        let accent = accent(&theme);
        let working = self.chat.busy && steps.turn == self.newest_turn();
        let count = plural(steps.items.len(), "step", "steps");
        let phases = self.phase_starts().len();

        // The icon of the steps, in a tinted square, as in the document panel.
        let badge = div()
            .size(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .bg(accent.opacity(0.12))
            .child(Icon::new(KitIcon::Layers).size(px(15.)).text_color(accent));
        let heading = v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_sm().font_semibold().child("Steps"))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(if working { format!("Phase {phases} · {count}") } else { format!("{} · {count}", plural(phases, "phase", "phases")) }),
            );
        let header = self
            .toolbar("steps-toolbar", cx)
            .h(px(TOOLBAR))
            .pl_3()
            .pr_1p5()
            .gap_2p5()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(badge)
            .child(heading)
            .child(
                Button::new("close-steps")
                    .ghost()
                    .small()
                    .icon(KitIcon::X)
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| this.close_steps(cx))),
            );

        let body = if steps.items.is_empty() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if working { "The steps show here when they come." } else { "This turn has no steps." })
                .into_any_element()
        } else {
            MessageScroller::new(
                "steps",
                steps.scroller.clone(),
                cx.processor(|this, row: usize, _, cx| {
                    this.render_step(row, cx)
                }),
            )
            .flex_1()
            .min_h_0()
            .scrollbar(false)
            .with_list_style(StyleRefinement::default().py_4())
            .with_row_style(StyleRefinement::default().px_0().pb_0())
            .with_bottom_fade(theme.background)
            .into_any_element()
        };

        let card = v_flex()
            .size_full()
            .rounded(px(16.))
            .overflow_hidden()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(header)
            .child(body);
        Some(self.side_panel("steps-panel", self.steps_width, WIDTH, |this, w| this.steps_width = w, card, window, cx))
    }

    /// The phases of the steps: the first row of each phase. A note of the agent starts a new
    /// phase, and the note is the summary of the phase. The steps before the first note are the
    /// first phase.
    fn phase_starts(&self) -> Vec<usize> {
        let Some(steps) = &self.steps else { return Vec::new() };
        steps
            .items
            .iter()
            .enumerate()
            .filter(|&(row, &ix)| row == 0 || matches!(self.chat.items.get(ix), Some(Item::Agent(_))))
            .map(|(row, _)| row)
            .collect()
    }

    /// One row of the panel. The first row of a phase has the header of the phase. A tool is a
    /// line in the list of its phase, and a click on the line opens its output.
    fn render_step(&self, row: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(steps) = &self.steps else { return div().into_any_element() };
        let Some((ix, item)) = steps.items.get(row).and_then(|&ix| Some((ix, self.chat.items.get(ix)?))) else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let starts = self.phase_starts();
        let phase = starts.iter().rposition(|&s| s <= row).unwrap_or(0);
        let start = starts[phase];
        let end = starts.get(phase + 1).copied().unwrap_or(steps.items.len());
        let last_phase = phase + 1 == starts.len();
        let working = last_phase && self.chat.busy && steps.turn == self.newest_turn();

        // The header of the phase: a number, or a check mark when the phase is done, and the
        // kinds of the tools of the phase.
        let header = (row == start).then(|| {
            let kinds: Vec<&str> = steps.items[start..end]
                .iter()
                .filter_map(|&i| match self.chat.items.get(i) {
                    Some(Item::Tool { kind, .. }) => Some(kind.as_str()),
                    _ => None,
                })
                .collect();
            let failed = steps.items[start..end]
                .iter()
                .any(|&i| matches!(self.chat.items.get(i), Some(Item::Tool { status: ToolStatus::Failed, .. })));
            let (badge_fg, badge_bg) = if working {
                (accent(&theme), accent(&theme).opacity(0.18))
            } else if failed {
                (theme.danger, theme.danger.opacity(0.15))
            } else {
                (theme.success, theme.success.opacity(0.15))
            };
            let badge = div()
                .size(px(BADGE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(badge_bg)
                .text_color(badge_fg)
                .text_xs()
                .font_semibold()
                .map(|el| {
                    if working {
                        el.child(format!("{}", phase + 1))
                    } else if failed {
                        el.child(Icon::new(KitIcon::X).size(px(11.)))
                    } else {
                        el.child(Icon::new(KitIcon::Check).size(px(11.)))
                    }
                });
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(badge)
                .child(div().flex_1().min_w_0().truncate().child(phase_label(phase, &kinds, working)))
                .when(working, |el| el.child(Spinner::new().with_size(px(12.)).color(accent(&theme))))
        });

        let body = match item {
            // The note of the agent is the summary of the phase.
            Item::Agent(text) => div()
                .pl(px(INDENT))
                .pt_1p5()
                .pb_2()
                .text_sm()
                .child(TextView::markdown(self.text_id(ix), SharedString::from(text.clone())).style(message_style()).selectable(true))
                .into_any_element(),
            Item::Tool { id, title, kind, status, detail, files, .. } => {
                let open = detail.is_some() && (self.toggled_tools.contains(id) != (*status == ToolStatus::Failed));
                let (verb, subject) = step_title(kind, title);
                let mono = kind.eq_ignore_ascii_case("execute");
                let group: SharedString = format!("step-{ix}").into();
                let key = id.clone();
                let color = if *status == ToolStatus::Failed { theme.danger } else { theme.muted_foreground };
                let line = h_flex()
                    .id(("step", ix))
                    .group(group.clone())
                    .py_1()
                    .gap_2()
                    .text_sm()
                    .text_color(color)
                    .hover(|s| s.text_color(theme.foreground))
                    .when(detail.is_some(), |el| {
                        el.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_tool(ix, &key, cx)))
                    })
                    // A long title, such as a command, is cut. The tooltip has all of it.
                    .when(title.chars().count() > LONG_TITLE, |el| {
                        let title = title.clone();
                        el.tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(title.clone()).build(window, cx))
                    })
                    .child(Icon::new(tool_icon(kind)).size(px(13.)))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().flex_shrink_0().child(verb))
                            .when(!subject.is_empty(), |el| {
                                el.child(div().flex_shrink_0().child("·")).child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .when(mono, |el| el.text_xs().font_family(theme.mono_font_family.clone()))
                                        .child(subject),
                                )
                            }),
                    )
                    // A step that works or failed shows its state. A step that is done shows
                    // nothing, because the header of the phase shows it.
                    .when(*status == ToolStatus::Running, |el| {
                        el.child(Spinner::new().with_size(px(12.)).color(accent(&theme)))
                    })
                    .when(detail.is_some(), |el| {
                        el.child(
                            div()
                                .flex_shrink_0()
                                .invisible()
                                .group_hover(group.clone(), |s| s.visible())
                                .when(open, |el| el.visible())
                                .child(Icon::new(if open { KitIcon::ChevronDown } else { KitIcon::ChevronRight }).size(px(14.))),
                        )
                    });
                v_flex()
                    .ml(px(INDENT))
                    .pl_3()
                    .border_l_1()
                    .border_color(theme.border)
                    .child(line)
                    // The images of the tool always show, for example the screenshots of the agent Mac.
                    .when(!files.is_empty(), |el| {
                        el.child(div().pb_1().child(attach::tiles(format!("step-files-{ix}"), files, &self.env.files_dir, false, cx)))
                    })
                    .when_some(detail.as_ref().filter(|_| open), |el, d| {
                        let mut short: String = d.chars().take(TOOL_DETAIL).collect();
                        // Show that the output has more text than shows.
                        if short.len() < d.len() {
                            short.push_str("\n…");
                        }
                        el.child(
                            div()
                                .id(("step-detail", ix))
                                .mb_1()
                                .px_3()
                                .py_2()
                                .max_h(px(240.))
                                .overflow_y_scroll()
                                .rounded_md()
                                .border_1()
                                .border_color(theme.border.opacity(0.6))
                                .bg(theme.foreground.opacity(0.03))
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(short),
                        )
                    })
                    .into_any_element()
            }
            Item::Thought(text) => h_flex()
                .ml(px(INDENT))
                .pl_3()
                .py_1()
                .gap_2()
                .items_start()
                .border_l_1()
                .border_color(theme.border)
                .text_sm()
                .italic()
                .text_color(theme.muted_foreground)
                .child(Icon::new(KitIcon::Brain).size(px(13.)).mt_0p5())
                .child(div().flex_1().min_w_0().child(text.clone()))
                .into_any_element(),
            // A plan is a card in the list. The chat card has a margin for the tail of the bubbles.
            item => div()
                .ml(px(INDENT))
                .pl_3()
                .py_1()
                .border_l_1()
                .border_color(theme.border)
                .child(div().ml(px(-super::TAIL_INSET)).child(self.render_item(ix, item, None, cx)))
                .into_any_element(),
        };

        v_flex()
            .w_full()
            .px_4()
            // Space between the phases.
            .when(row == start && row > 0, |el| el.pt_4())
            .when_some(header, |el, header| el.child(header))
            // The header of a phase with no note has a little space above its list.
            .when(row == start && !matches!(item, Item::Agent(_)), |el| el.child(div().h(px(6.))))
            .child(body)
            .into_any_element()
    }
}

/// The label of a phase: the kinds of its tools, for example "2 searches · 3 pages".
fn phase_label(phase: usize, kinds: &[&str], working: bool) -> String {
    let mut counts: Vec<(&str, &str, usize)> = Vec::new();
    for kind in kinds {
        let (one, many) = match kind.to_ascii_lowercase().as_str() {
            "search" => ("search", "searches"),
            "fetch" => ("page", "pages"),
            "read" => ("file read", "files read"),
            "edit" | "delete" | "move" => ("edit", "edits"),
            "execute" => ("command", "commands"),
            "think" => ("thought", "thoughts"),
            _ => ("tool", "tools"),
        };
        match counts.iter_mut().find(|(o, _, _)| *o == one) {
            Some(c) => c.2 += 1,
            None => counts.push((one, many, 1)),
        }
    }
    let mut label = format!("Phase {}", phase + 1);
    if counts.is_empty() {
        if working {
            label.push_str(" · Working");
        }
        return label;
    }
    for (one, many, n) in counts {
        label.push_str(" · ");
        label.push_str(&plural(n, one, many));
    }
    label
}

/// The verb and the subject of a tool title, for example ("Search", "\"ACOA programs\"") or
/// ("Run", "cd ~/Library"). A path in the home folder starts with "~", and a URL loses its
/// scheme, so that more of the subject shows.
fn step_title(kind: &str, title: &str) -> (String, String) {
    let (verb, subject) = if kind.eq_ignore_ascii_case("execute") {
        ("Run", title)
    } else {
        match title.split_once(' ') {
            Some((verb, rest))
                if verb.len() <= 12 && verb.starts_with(|c: char| c.is_ascii_uppercase()) && verb.chars().all(|c| c.is_ascii_alphabetic()) =>
            {
                (verb, rest)
            }
            _ => (title, ""),
        }
    };
    let mut subject = subject.trim().to_string();
    for scheme in ["https://www.", "http://www.", "https://", "http://"] {
        if let Some(rest) = subject.strip_prefix(scheme) {
            subject = rest.to_string();
            break;
        }
    }
    if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()).filter(|h| h.len() > 1) {
        subject = subject.replace(&home, "~");
    }
    (verb.to_string(), subject)
}

#[cfg(test)]
mod tests {
    use super::{phase_label, step_title};

    #[test]
    fn labels_phases() {
        assert_eq!(phase_label(0, &["Search", "Search", "Fetch"], false), "Phase 1 · 2 searches · 1 page");
        assert_eq!(phase_label(2, &[], true), "Phase 3 · Working");
    }

    #[test]
    fn splits_titles() {
        assert_eq!(step_title("Search", "Search \"ACOA\""), ("Search".into(), "\"ACOA\"".into()));
        assert_eq!(step_title("Fetch", "Fetch https://www.gov.nl.ca/iet"), ("Fetch".into(), "gov.nl.ca/iet".into()));
        assert_eq!(step_title("Execute", "cd /tmp"), ("Run".into(), "cd /tmp".into()));
        assert_eq!(step_title("Other", "ToolSearch"), ("ToolSearch".into(), String::new()));
        assert_eq!(step_title("Other", "mcp__x__y do"), ("mcp__x__y do".into(), String::new()));
    }
}
