//! Sample chats for screenshots and design work. Start the app with `AGENT_DESK_DEMO=1`.
//! The sample chats have no agent process, so the app sends no prompts.

use acp_host::{AgentKind, PermissionOptionView, ToolStatus};
use chrono::{Duration, Local};
use tokio::sync::oneshot;

use super::{Chat, Item, PendingPermission};

pub fn enabled() -> bool {
    std::env::var_os("AGENT_DESK_DEMO").is_some()
}

/// A sample chat. `minutes` is the time since its last activity.
fn chat(id: u64, kind: AgentKind, title: &str, minutes: i64, items: Vec<Item>) -> Chat {
    Chat {
        id,
        kind,
        title: title.into(),
        items,
        session: None,
        busy: false,
        closed: false,
        permission: None,
        updated: Local::now() - Duration::minutes(minutes),
        draft: String::new(),
    }
}

fn tool(id: &str, title: &str, kind: &str, status: ToolStatus, detail: Option<&str>) -> Item {
    Item::Tool {
        id: id.into(),
        title: title.into(),
        kind: kind.into(),
        status,
        detail: detail.map(Into::into),
    }
}

pub fn chats() -> Vec<Chat> {
    let mut working = chat(
        1,
        AgentKind::Claude,
        "Fix the flaky login test",
        1,
        vec![
            Item::User("The login test fails about one time in five on CI. Can you find out why and fix it?".into()),
            Item::Thought("The test probably races with the session cookie. Look at the setup first.".into()),
            tool(
                "t1",
                "Read tests/login.spec.ts",
                "read",
                ToolStatus::Done,
                Some("test('logs in', async ({ page }) => {\n  await page.goto('/login');\n  await page.fill('#email', user.email);\n  await page.click('text=Sign in');\n  expect(page.url()).toContain('/home');\n});"),
            ),
            Item::Agent(
                "I found the problem. The test reads `page.url()` **before** the redirect finishes:\n\n\
                 ```ts\nawait page.click('text=Sign in');\nexpect(page.url()).toContain('/home');\n```\n\n\
                 On a slow CI machine the URL is still `/login`. I will wait for the navigation instead."
                    .into(),
            ),
            Item::Plan(vec![
                ("Find the cause of the failure".into(), ToolStatus::Done),
                ("Wait for the redirect in the test".into(), ToolStatus::Done),
                ("Run the test 50 times to confirm the fix".into(), ToolStatus::Running),
            ]),
            tool("t2", "Edit tests/login.spec.ts", "edit", ToolStatus::Done, None),
            tool("t3", "npx playwright test login --repeat-each=50", "execute", ToolStatus::Running, None),
        ],
    );
    working.busy = true;

    let (reply, _) = oneshot::channel();
    let mut waiting = chat(
        2,
        AgentKind::Codex,
        "Add a dark mode to the settings page",
        12,
        vec![
            Item::User("Add a dark mode switch to the settings page. Keep the choice in local storage.".into()),
            Item::Agent("I will add a `ThemeSwitch` component and a `useTheme` hook. First I need to install a package.".into()),
            tool("t1", "Search for \"settings\" in src/", "search", ToolStatus::Done, None),
        ],
    );
    waiting.busy = true;
    waiting.permission = Some(PendingPermission {
        title: "Run `npm install next-themes`".into(),
        options: vec![
            PermissionOptionView { id: "allow".into(), name: "Allow".into(), allow: true },
            PermissionOptionView { id: "always".into(), name: "Always allow".into(), allow: true },
            PermissionOptionView { id: "reject".into(), name: "Reject".into(), allow: false },
        ],
        reply,
    });

    let done = chat(
        3,
        AgentKind::Claude,
        "Summarize the README",
        1560,
        vec![
            Item::User("Summarize the README in three points.".into()),
            Item::Agent(
                "Agent Desk is a desktop app to talk to coding agents:\n\n\
                 1. **Agents** — it runs Claude Code and Codex on your Mac through the Agent Client Protocol.\n\
                 2. **Agent Mac** — the agents can use their own macOS VM, so computer use does not take over your screen.\n\
                 3. **Tools** — an MCP bridge gives the agents `vm_shell`, `vm_screenshot`, `vm_click`, and more."
                    .into(),
            ),
        ],
    );

    let mut failed = chat(
        4,
        AgentKind::Codex,
        "Deploy the preview build",
        4320,
        vec![
            Item::User("Deploy the preview build to staging.".into()),
            tool("t1", "./scripts/deploy.sh staging", "execute", ToolStatus::Failed, Some("error: no credentials for staging")),
            Item::Error("The deploy script stopped: no credentials for staging.".into()),
            Item::Notice("The agent process stopped.".into()),
        ],
    );
    failed.closed = true;

    let empty = chat(5, AgentKind::Claude, "New chat", 0, vec![]);

    vec![working, waiting, done, failed, empty]
}
