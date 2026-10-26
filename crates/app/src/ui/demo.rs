//! A sample conversation for screenshots and design work. Start the app with `HELP_DEMO=1`.
//! The sample conversation has no agent process. The app starts no agent for it.
//! With `HELP_DEMO=live`, a message from the user plays a sample turn in which the agent
//! delegates work to subagents.
//! With `HELP_DEMO_APP=<server>/<tool>`, the sample conversation ends with a call of that MCP
//! tool. The app connects to the MCP server, and shows the view of the tool if it has one.

use std::time::Duration;

use acp_host::{AgentKind, Event, McpCall, PermissionOptionView, SUBAGENT_KIND, ToolStatus};
use tokio::sync::oneshot;

use super::{Chat, Item, PendingPermission};

pub fn enabled() -> bool {
    std::env::var_os("HELP_DEMO").is_some()
}

/// The events of the MCP tool call of `HELP_DEMO_APP`. The result is a sample: the time now.
pub fn app_call() -> Vec<Event> {
    let Some(name) = std::env::var("HELP_DEMO_APP").ok().filter(|_| enabled()) else { return Vec::new() };
    let Some((server, tool)) = name.split_once('/') else { return Vec::new() };
    let time = chrono::Utc::now().to_rfc3339();
    vec![
        Event::ToolCall {
            id: "app1".into(),
            title: format!("mcp__{server}__{tool}"),
            kind: "Other".into(),
            status: ToolStatus::Running,
            parent: None,
            mcp: Some(McpCall { server: server.into(), tool: tool.into() }),
            input: Some(serde_json::json!({})),
        },
        Event::ToolUpdate {
            id: "app1".into(),
            title: None,
            status: Some(ToolStatus::Done),
            detail: Some(time.clone()),
            files: Vec::new(),
            input: None,
            output: Some(serde_json::json!({
                "content": [{ "type": "text", "text": time }],
                "structuredContent": { "time": time },
            })),
        },
    ]
}

/// A message from the user plays the sample turn of `delegation`.
pub fn live() -> bool {
    std::env::var_os("HELP_DEMO").is_some_and(|v| v == "live")
}

fn tool(id: &str, title: &str, kind: &str, status: ToolStatus, detail: Option<&str>) -> Item {
    child(id, title, kind, status, detail, None)
}

/// A tool call that the subagent `parent` made.
fn child(id: &str, title: &str, kind: &str, status: ToolStatus, detail: Option<&str>, parent: Option<&str>) -> Item {
    Item::Tool {
        id: id.into(),
        title: title.into(),
        kind: kind.into(),
        status,
        detail: detail.map(Into::into),
        parent: parent.map(Into::into),
        files: Vec::new(),
    }
}

/// A message from the user, `ago` minutes before now. Its turn took 1m 27s.
fn user(text: &str, ago: i64) -> Item {
    Item::User { text: text.into(), at: chrono::Utc::now().timestamp() - ago * 60, attachments: Vec::new(), took: Some(87) }
}

/// A sample document for the long reply of the demo. See `doc::links`.
const REVIEW: &str = "# Settings review\n\n\
## Summary\n\nThe settings code works, but two options have no checks.\n\n\
## Findings\n\n\
1. **`theme`** — the app accepts any string. Accept only `light`, `dark`, and `system`.\n\
2. **`timeout`** — a negative value stops all requests. Set a minimum of 1 second.\n\n\
## Next steps\n\n- Add a schema for the settings file.\n- Add a test for each option.\n";

/// Write the sample document if it is not there, and give its path.
fn review(home: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(home).join("Library/Application Support/Help/documents/settings-review.md");
    if !path.exists() {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(&path, REVIEW);
    }
    path
}

/// Five turns over some days. The agent waits for a permission in the last turn.
pub fn chat() -> Chat {
    let home = std::env::var("HOME").unwrap_or_default();
    let items = vec![
        user("Summarize the README in three points.", 9 * 24 * 60),
        Item::Agent(
            "Help is a desktop app to talk to coding agents:\n\n\
             1. **Agents** — it runs Claude Code and Codex on your Mac through the Agent Client Protocol.\n\
             2. **Agent Mac** — the agents can use their own macOS VM, so computer use does not take over your screen.\n\
             3. **Tools** — an MCP bridge gives the agents `vm_shell`, `vm_screenshot`, `vm_click`, and more."
                .into(),
        ),
        user("Deploy the preview build to staging.", 26 * 60),
        tool("d1", "./scripts/deploy.sh staging", "execute", ToolStatus::Failed, Some("error: no credentials for staging")),
        Item::Error("The deploy script stopped: no credentials for staging.".into()),
        user("Review the settings code and write a report.", 5 * 60),
        Item::Agent(format!(
            "The settings code works, but two options have no checks: `theme` and `timeout`. \
             The report has the findings and the next steps.\n\n[Settings review](<{}>)",
            review(&home).display()
        )),
        user("The login test fails about one time in five on CI. Can you find out why and fix it?", 3 * 60),
        Item::Thought("The test probably races with the session cookie. Look at the setup first.".into()),
        tool(
            "f1",
            "Read tests/login.spec.ts",
            "read",
            ToolStatus::Done,
            Some("test('logs in', async ({ page }) => {\n  await page.goto('/login');\n  await page.fill('#email', user.email);\n  await page.click('text=Sign in');\n  expect(page.url()).toContain('/home');\n});"),
        ),
        Item::Agent(
            "I found the problem. The test reads `page.url()` **before** the redirect finishes:\n\n\
             ```ts\nawait page.click('text=Sign in');\nexpect(page.url()).toContain('/home');\n```\n\n\
             On a slow CI machine the URL is still `/login`. I waited for the navigation instead."
                .into(),
        ),
        Item::Plan(vec![
            ("Find the cause of the failure".into(), ToolStatus::Done),
            ("Wait for the redirect in the test".into(), ToolStatus::Done),
            ("Run the test 50 times to confirm the fix".into(), ToolStatus::Done),
        ]),
        tool("f2", "Edit tests/login.spec.ts", "edit", ToolStatus::Done, None),
        tool("f3", "npx playwright test login --repeat-each=50", "execute", ToolStatus::Done, None),
        Item::Agent("The test passed 50 times in a row. The fix is in `tests/login.spec.ts`.".into()),
        user("Find where the app reads its settings, and check the docs for each option.", 50),
        Item::Agent("I will delegate this to two subagents: one reads the code, and one reads the docs.".into()),
        tool("s1", "Find where the settings are read", acp_host::SUBAGENT_KIND, ToolStatus::Done, Some("The settings are read in `src/config.ts`.")),
        child("s1a", "Search for \"settings\" in src/", "search", ToolStatus::Done, None, Some("s1")),
        child("s1b", "Read src/config.ts", "read", ToolStatus::Done, None, Some("s1")),
        tool("s2", "Check the docs for each option", acp_host::SUBAGENT_KIND, ToolStatus::Done, Some("All 6 options have docs.")),
        child("s2a", "Read docs/settings.md", "read", ToolStatus::Done, None, Some("s2")),
        Item::Agent("The app reads its settings in `src/config.ts`. The docs cover all 6 options.".into()),
        user("Add a dark mode switch to the settings page. Keep the choice in local storage.", 2),
        Item::Agent("I will add a `ThemeSwitch` component and a `useTheme` hook. First I need to install a package.".into()),
        tool("w1", "Search for \"settings\" in src/", "search", ToolStatus::Done, None),
    ];
    let mut chat = Chat::new(AgentKind::Claude, items, std::path::PathBuf::from(home).join("Projects/web-app"));
    if live() {
        // The live demo waits for a message, so the last turn and its permission request go.
        chat.items.truncate(chat.items.len() - 3);
        return chat;
    }
    let (reply, _) = oneshot::channel();
    chat.busy = true;
    chat.permission = Some(PendingPermission {
        title: "Run `npm install next-themes`".into(),
        options: vec![
            PermissionOptionView { id: "allow".into(), name: "Allow".into(), allow: true },
            PermissionOptionView { id: "always".into(), name: "Always allow".into(), allow: true },
            PermissionOptionView { id: "reject".into(), name: "Reject".into(), allow: false },
        ],
        reply,
    });
    chat
}

fn call(id: &str, title: &str, kind: &str, parent: Option<&str>) -> Event {
    Event::ToolCall {
        id: id.into(),
        title: title.into(),
        kind: kind.into(),
        status: ToolStatus::Running,
        parent: parent.map(Into::into),
        mcp: None,
        input: None,
    }
}

fn done(id: &str, detail: Option<&str>) -> Event {
    Event::ToolUpdate {
        id: id.into(),
        title: None,
        status: Some(ToolStatus::Done),
        detail: detail.map(Into::into),
        files: Vec::new(),
        input: None,
        output: None,
    }
}

/// A sample turn: the agent delegates a review to three subagents, and each event comes after
/// its delay.
/// `HELP_DEMO_SLOW=4` makes each delay four times longer, for a recording with few frames.
pub fn delegation() -> Vec<(Duration, Event)> {
    let slow: u64 = std::env::var("HELP_DEMO_SLOW").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let ms = |n: u64| Duration::from_millis(n * slow);
    let text = |t: &str| Event::AgentText(t.into());
    vec![
        (ms(1200), text("I will delegate this to three subagents that work at the same time:\n\n")),
        (ms(300), text("- one reads the login code\n- one reads the tests\n- one checks the open issues\n")),
        (ms(800), call("a", "Read the login code", SUBAGENT_KIND, None)),
        (ms(400), call("b", "Read the login tests", SUBAGENT_KIND, None)),
        (ms(400), call("c", "Check the open issues", SUBAGENT_KIND, None)),
        (ms(900), call("a1", "Search for \"login\" in src/", "search", Some("a"))),
        (ms(500), call("b1", "Read tests/login.spec.ts", "read", Some("b"))),
        (ms(500), call("c1", "gh issue list --label login", "execute", Some("c"))),
        (ms(900), done("a1", None)),
        (ms(100), call("a2", "Read src/auth/session.ts", "read", Some("a"))),
        (ms(800), done("b1", None)),
        (ms(100), call("b2", "npx playwright test login", "execute", Some("b"))),
        (ms(900), done("c1", None)),
        (ms(100), call("c2", "gh issue view 142", "execute", Some("c"))),
        (ms(1000), done("a2", None)),
        (ms(100), call("a3", "Read src/auth/cookie.ts", "read", Some("a"))),
        (ms(1200), done("c2", None)),
        (ms(200), done("c", Some("Issue #142: users see the login page again after a sign-in on Safari."))),
        (ms(1000), done("a3", None)),
        (ms(200), done("a", Some("The session cookie has `SameSite=None` but no `Secure` flag."))),
        (ms(1400), done("b2", None)),
        (ms(200), done("b", Some("All 12 login tests pass. No test uses Safari."))),
        (ms(900), text("All three subagents finished. The login bug is in `src/auth/cookie.ts`: ")),
        (ms(300), text("the session cookie has `SameSite=None` but no `Secure` flag, so Safari drops it ")),
        (ms(300), text("(issue #142). The tests do not catch it, because no test uses Safari.")),
        (ms(500), Event::TurnEnded("EndTurn".into())),
    ]
}

/// The sets of sample data. A dev build shows a switch for them in the demo. Start the app with
/// `HELP_DEMO=worst`, `empty`, `one`, or `huge` to start with that set.
#[derive(Clone, Copy, PartialEq)]
pub enum Data {
    /// The sample conversation of `chat`.
    Demo,
    /// The longest, shortest, and strangest values that real data can have. See `worst`.
    Worst,
    /// A conversation with no messages.
    Empty,
    /// Each count is 1.
    One,
    /// 1,000 turns.
    Huge,
}

impl Data {
    pub const ALL: [Data; 5] = [Data::Demo, Data::Worst, Data::Empty, Data::One, Data::Huge];

    pub fn initial() -> Self {
        match std::env::var("HELP_DEMO").as_deref() {
            Ok("worst") => Data::Worst,
            Ok("empty") => Data::Empty,
            Ok("one") => Data::One,
            Ok("huge") => Data::Huge,
            _ => Data::Demo,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Data::Demo => "Demo",
            Data::Worst => "Worst case",
            Data::Empty => "Empty",
            Data::One => "One",
            Data::Huge => "1,000 turns",
        }
    }

    pub fn chat(self) -> Chat {
        let home = std::env::var("HOME").unwrap_or_default();
        let cwd = std::path::PathBuf::from(&home).join("Projects/web-app");
        match self {
            Data::Demo => chat(),
            Data::Worst => worst(&home),
            Data::Empty => Chat::new(AgentKind::Claude, Vec::new(), cwd),
            Data::One => one(cwd),
            Data::Huge => huge(cwd),
        }
    }

    /// The MCP servers with views in the sidebar: (server, title).
    pub fn app_servers(self) -> Vec<(&'static str, &'static str)> {
        match self {
            Data::Worst => vec![
                ("northwind-industries-holdings-analytics-dashboard", "Northwind Industries Holdings Analytics Dashboard"),
                ("fox", "🦊 Fox"),
                ("wang", "王秀英"),
                ("spaces", "  Sam   Lee "),
                ("untitled", ""),
                ("figma", "Figma"),
                ("linear", "Linear"),
                ("notion", "Notion"),
                ("benachrichtigungen", "Benachrichtigungseinstellungen"),
            ],
            Data::One => vec![("figma", "Figma")],
            _ => Vec::new(),
        }
    }

    /// The packages of command line tools in the sidebar. See `clis`.
    pub(super) fn clis(self) -> Vec<super::clis::Cli> {
        let cli = |name: &str, commands: &[&str]| super::clis::Cli {
            name: name.into(),
            commands: commands.iter().map(|c| c.to_string()).collect(),
        };
        match self {
            Data::Worst => {
                // GNU coreutils has about 100 commands, each with a `g` before its name.
                let coreutils: Vec<String> = ["base64", "basename", "cat", "chmod", "chown", "cp", "cut", "date", "dd", "df", "dirname", "du", "echo", "env", "expand", "expr", "factor", "head", "id", "join", "kill", "link", "ln", "ls", "md5sum", "mkdir", "mktemp", "mv", "nice", "nl", "nohup", "numfmt", "od", "paste", "pr", "printf", "pwd", "readlink", "realpath", "rm", "rmdir", "seq", "sha1sum", "sha256sum", "shuf", "sleep", "sort", "split", "stat", "stdbuf", "tail", "tee", "test", "timeout", "touch", "tr", "truncate", "tsort", "tty", "uname", "uniq", "unlink", "users", "wc", "who", "whoami", "yes"]
                    .iter()
                    .map(|c| format!("g{c}"))
                    .collect();
                let coreutils: Vec<&str> = coreutils.iter().map(String::as_str).collect();
                vec![
                    cli("@modelcontextprotocol/server-filesystem", &["mcp-server-filesystem"]),
                    cli("@anthropic-ai/claude-code", &["claude"]),
                    cli("coreutils", &coreutils),
                    cli("gh", &["gh"]),
                    cli("google-cloud-sdk", &["bq", "docker-credential-gcloud", "gcloud", "git-credential-gcloud.sh", "gsutil"]),
                    cli("jq", &["jq"]),
                    cli("k9s", &["k9s"]),
                    cli("Visual Studio Code - Insiders", &["code-insiders"]),
                    cli("Python", &["idle3.13", "pip3", "pip3.13", "pydoc3", "pydoc3.13", "python3", "python3-config", "python3.13", "python3.13-config"]),
                ]
            }
            Data::One => vec![cli("gh", &["gh"])],
            Data::Huge => (1..=300).map(|i| cli(&format!("package-{i}"), &["command"])).collect(),
            _ => Vec::new(),
        }
    }

    /// The files in the composer, for the next message.
    pub fn composer_files(self) -> Vec<Attachment> {
        let home = std::env::var("HOME").unwrap_or_default();
        match self {
            Data::Worst => {
                let dir = worst_dir(&home);
                vec![
                    file(&dir, "Q3 Board Deck — FINAL (revised) v12 [approved by legal].pdf", "application/pdf", 18_432_000),
                    file(&dir, "IMG_20250914_183022_HDR_portrait_edited_edited.HEIC", "image/heic", 4_812_000),
                    image_file(&dir, "panorama.png", 4000, 200),
                ]
            }
            _ => Vec::new(),
        }
    }
}

use acp_host::Attachment;

fn worst_dir(home: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(home).join("Library/Application Support/Help/demo-worst");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// An attachment for a file that is not on the disk.
fn file(dir: &std::path::Path, name: &str, mime: &str, size: u64) -> Attachment {
    Attachment { path: dir.join(name), name: name.into(), mime: mime.into(), size }
}

/// An attachment for a PNG file of the given size. The function writes the file if it is not there.
fn image_file(dir: &std::path::Path, name: &str, w: u32, h: u32) -> Attachment {
    let path = dir.join(name);
    if !path.exists() {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 255 / w) as u8, (y * 255 / h) as u8, 160, 255]));
        let _ = img.save(&path);
    }
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    Attachment { path, name: name.into(), mime: "image/png".into(), size }
}

/// A document with the given name and text. The function writes the file if it is not there.
fn document(home: &str, name: &str, text: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(home).join("Library/Application Support/Help/documents").join(name);
    if !path.exists() {
        let _ = path.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(&path, text);
    }
    path
}

/// The worst realistic values for each field that the conversation shows, spread over the turns.
fn worst(home: &str) -> Chat {
    let dir = worst_dir(home);
    let long_doc = document(
        home,
        "parametres-de-confidentialite-et-de-securite-rapport-complet-v12.md",
        "# Paramètres de confidentialité et de sécurité — rapport complet v12\n\nLe rapport complet.\n",
    );
    let missing_doc = std::path::Path::new(home).join("Library/Application Support/Help/documents/deleted-report.md");
    let minified = format!(
        "{{\"data\":[{}]}}",
        (0..80)
            .map(|i| format!("{{\"id\":\"9f8e7d6c-5b4a-4c3d-8e2f-1a0b9c8d7e{i:02}\",\"email\":\"bartholomew.fitzgerald+{i}@northwind-industries-holdings.example.com\"}}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let heredoc = format!(
        "cat > /Users/aleksandra.wisniewska-kowalczyk/Projects/northwind-industries-holdings/web-app/.env.production <<'EOF'\n{}\nEOF",
        (0..40).map(|i| format!("SERVICE_{i}_URL=https://example.com/workspaces/acme/projects/q3-launch/services/{i}")).collect::<Vec<_>>().join("\n")
    );
    let items = vec![
        // Three years ago: the time label has the year. Attachments only, and no text.
        Item::User {
            text: String::new(),
            at: chrono::Utc::now().timestamp() - 3 * 365 * 24 * 60 * 60,
            attachments: vec![
                file(&dir, "Q3 Board Deck — FINAL (revised) v12 [approved by legal].pdf", "application/pdf", 18_432_000),
                file(&dir, "Q3 Board Deck — FINAL (revised) v11 [approved by legal].pdf", "application/pdf", 18_431_000),
                image_file(&dir, "tall-screenshot.png", 200, 4000),
                image_file(&dir, "panorama.png", 4000, 200),
            ],
            took: None,
        },
        Item::Agent("Done.".into()),
        // An unbreakable URL and email, Markdown characters, and HTML.
        user(
            "Why does https://example.com/workspaces/acme/projects/q3-launch/docs/9f8e7d6c5b4a?tab=comments&filter=unresolved \
             not send to bartholomew.fitzgerald@northwind-industries-holdings.example.com? \
             The form shows <script>alert(1)</script> and **bold** and &amp; as text.",
            26 * 60,
        ),
        Item::Thought(format!("Aleksandra Wiśniewska-Kowalczyk, Đặng Thị Ngọc Hân, 王秀英, نور الهدى عبد الرحمن. {}", "The user asks about the email form. ".repeat(12))),
        tool(
            "w1",
            "cd /Users/aleksandra.wisniewska-kowalczyk/Projects/northwind-industries-holdings/web-app && npx playwright test tests/e2e/checkout/payment-methods.spec.ts --project=webkit --repeat-each=50 --reporter=line",
            "execute",
            ToolStatus::Done,
            None,
        ),
        tool("w2", "Read src/api/members.json", "read", ToolStatus::Failed, Some(&minified)),
        tool("w3", "Đặng Thị Ngọc Hân — Benachrichtigungseinstellungen", "edit", ToolStatus::Done, None),
        Item::Plan(Vec::new()),
        Item::Plan(
            [
                "Find the cause",
                "Read the form code in src/components/forms/ContactForm.tsx and the server action in src/app/actions/contact.ts",
                "Benachrichtigungseinstellungen",
                "Fix the escaping",
                "Add a test",
                "Run the tests",
                "Update the docs",
                "Ask for a review",
                "Deploy to staging",
                "Check the logs",
                "Deploy to production",
                "Close issue #142",
            ]
            .iter()
            .enumerate()
            .map(|(i, t)| (t.to_string(), if i < 4 { ToolStatus::Done } else if i == 4 { ToolStatus::Running } else { ToolStatus::Pending }))
            .collect(),
        ),
        Item::Agent(
            "The form escapes the text twice. Compare:\n\n\
             | Field | Value before | Value after | Escaped | Source file |\n|---|---|---|---|---|\n\
             | email | bartholomew.fitzgerald@northwind-industries-holdings.example.com | bartholomew.fitzgerald%40northwind-industries-holdings.example.com | yes | src/app/actions/contact.ts |\n\
             | name | Aleksandra Wiśniewska-Kowalczyk | Aleksandra Wi&#347;niewska-Kowalczyk | yes | src/components/forms/ContactForm.tsx |\n\n\
             ```ts\nconst url = `https://example.com/workspaces/${workspace}/projects/${project}/docs/${doc}?tab=comments&filter=unresolved&highlight=${encodeURIComponent(email)}`;\n```\n\n\
             Total: 1284 of 1000000 members, 0.30000000000000004 of them, NaN%."
                .into(),
        ),
        Item::Notice(
            "Cannot attach IMG_20250914_183022_HDR_portrait_edited_edited.HEIC, Q3 Board Deck — FINAL (revised) v12 [approved by legal].pdf, \
             9f8e7d6c-5b4a-4c3d-8e2f-1a0b9c8d7e6f.zip."
                .into(),
        ),
        Item::Error(
            "Error: ENOENT: no such file or directory, open '/Users/aleksandra.wisniewska-kowalczyk/Library/Application Support/Help/files/9f8e7d6c-5b4a-4c3d-8e2f-1a0b9c8d7e6f/IMG_20250914_183022_HDR_portrait_edited_edited.HEIC'"
                .into(),
        ),
        Item::Compacted,
        // Right-to-left text, and a reply in the same language.
        user("نور الهدى عبد الرحمن هنا. لماذا لا يصل البريد إلى العميل؟ جربت ثلاث مرات اليوم ولم يصل أي شيء.", 4 * 60),
        Item::Agent("لأن النموذج يرمّز عنوان البريد مرتين. أصلحت ذلك في `src/app/actions/contact.ts` وأضفت اختبارا.".into()),
        // Line breaks, and characters that Markdown reads as format.
        user("Steps to see it:\n- open the form\n1. type a name\n# 142 is still open\nsnake_case_name and 2*3*4 = 24\n===", 4 * 60 - 1),
        Item::Agent("Thanks, I see it.".into()),
        // A reply that is only links to documents: the text is empty when the links go.
        user("Write the full report.", 3 * 60),
        Item::Agent(format!("[Report](<{}>)\n\n[Old report](<{}>)", long_doc.display(), missing_doc.display())),
        Item::AgentFiles(vec![
            image_file(&dir, "tall-screenshot.png", 200, 4000),
            file(&dir, "9f8e7d6c-5b4a-4c3d-8e2f-1a0b9c8d7e6f.zip", "application/zip", 0),
        ]),
        // One letter.
        user("k", 2),
        tool(
            "s1",
            "Find every place that reads Benachrichtigungseinstellungen in the northwind-industries-holdings monorepo",
            acp_host::SUBAGENT_KIND,
            ToolStatus::Running,
            None,
        ),
        child("s1a", "Search for \"Benachrichtigungseinstellungen\" in packages/notifications/src/settings/", "search", ToolStatus::Running, None, Some("s1")),
    ];
    let mut chat = Chat::new(AgentKind::Claude, items, std::path::PathBuf::from(home).join("Projects/northwind-industries-holdings/web-app"));
    let (reply, _) = oneshot::channel();
    chat.busy = true;
    chat.permission = Some(PendingPermission {
        title: format!("Run `{heredoc}`"),
        options: vec![
            PermissionOptionView { id: "allow".into(), name: "Allow".into(), allow: true },
            PermissionOptionView {
                id: "always".into(),
                name: "Always allow cat in /Users/aleksandra.wisniewska-kowalczyk/Projects/northwind-industries-holdings/web-app".into(),
                allow: true,
            },
            PermissionOptionView { id: "reject".into(), name: "Reject".into(), allow: false },
        ],
        reply,
    });
    chat
}

/// Each count is 1: one step, one subagent, one plan entry.
fn one(cwd: std::path::PathBuf) -> Chat {
    let items = vec![
        user("Check the docs.", 1),
        Item::Plan(vec![("Read the docs".into(), ToolStatus::Done)]),
        tool("o1", "Check the docs", acp_host::SUBAGENT_KIND, ToolStatus::Running, None),
        child("o1a", "Read docs/settings.md", "read", ToolStatus::Done, None, Some("o1")),
    ];
    let mut chat = Chat::new(AgentKind::Claude, items, cwd);
    chat.busy = true;
    chat
}

/// 1,000 turns, each with a step and a reply.
fn huge(cwd: std::path::PathBuf) -> Chat {
    let mut items = Vec::new();
    for i in 0..1000i64 {
        items.push(user(&format!("Question {} of 1,000.", i + 1), (1000 - i) * 90));
        items.push(tool(&format!("h{i}"), "Read src/config.ts", "read", ToolStatus::Done, None));
        items.push(Item::Agent(format!("Answer {} of 1,000.", i + 1)));
    }
    Chat::new(AgentKind::Claude, items, cwd)
}
