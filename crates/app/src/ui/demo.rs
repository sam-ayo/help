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
    }
}

/// A message from the user, `ago` minutes before now.
fn user(text: &str, ago: i64) -> Item {
    Item::User { text: text.into(), at: chrono::Utc::now().timestamp() - ago * 60 }
}

/// Four turns over some days. The agent waits for a permission in the last turn.
pub fn chat() -> Chat {
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
    let home = std::env::var("HOME").unwrap_or_default();
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
