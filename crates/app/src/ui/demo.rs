//! A sample conversation for screenshots and design work. Start the app with `AGENT_DESK_DEMO=1`.
//! The sample conversation has no agent process. The app starts no agent for it.

use acp_host::{AgentKind, PermissionOptionView, ToolStatus};
use tokio::sync::oneshot;

use super::{Chat, Item, PendingPermission};

pub fn enabled() -> bool {
    std::env::var_os("AGENT_DESK_DEMO").is_some()
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

/// Four earlier topics, and the agent waits for a permission in the last topic.
pub fn chat() -> Chat {
    let items = vec![
        Item::Topic("Summarize the README".into()),
        Item::User("Summarize the README in three points.".into()),
        Item::Agent(
            "Agent Desk is a desktop app to talk to coding agents:\n\n\
             1. **Agents** — it runs Claude Code and Codex on your Mac through the Agent Client Protocol.\n\
             2. **Agent Mac** — the agents can use their own macOS VM, so computer use does not take over your screen.\n\
             3. **Tools** — an MCP bridge gives the agents `vm_shell`, `vm_screenshot`, `vm_click`, and more."
                .into(),
        ),
        Item::Topic("Deploy the preview build".into()),
        Item::User("Deploy the preview build to staging.".into()),
        tool("d1", "./scripts/deploy.sh staging", "execute", ToolStatus::Failed, Some("error: no credentials for staging")),
        Item::Error("The deploy script stopped: no credentials for staging.".into()),
        Item::Topic("Fix the flaky login test".into()),
        Item::User("The login test fails about one time in five on CI. Can you find out why and fix it?".into()),
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
        Item::Topic(String::new()),
        Item::User("Add a dark mode switch to the settings page. Keep the choice in local storage.".into()),
        Item::Agent("I will add a `ThemeSwitch` component and a `useTheme` hook. First I need to install a package.".into()),
        tool("w1", "Search for \"settings\" in src/", "search", ToolStatus::Done, None),
    ];
    let home = std::env::var("HOME").unwrap_or_default();
    let mut chat = Chat::new(AgentKind::Claude, items, std::path::PathBuf::from(home).join("Projects/web-app"));
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
