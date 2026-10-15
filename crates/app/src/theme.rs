//! The color themes: the Agent Desk rose palette and the shadcn/ui themes.
//! A theme is a base color, an optional accent color, and an appearance.

use std::path::PathBuf;

use gpui::{App, Global, Hsla, Window, rgba};
use gpui_component::{Theme, ThemeMode};

use crate::shadcn_themes;

/// A set of colors with the names of the shadcn/ui CSS variables, as 0xRRGGBBAA.
pub struct ThemeDef {
    pub key: &'static str,
    pub title: &'static str,
    pub light: &'static [(&'static str, u32)],
    pub dark: &'static [(&'static str, u32)],
}

impl ThemeDef {
    fn vars(&self, dark: bool) -> &'static [(&'static str, u32)] {
        if dark { self.dark } else { self.light }
    }

    /// The color that shows this theme in a menu.
    pub fn swatch(&self, var: &str) -> Hsla {
        lookup(self.light, var).unwrap_or(gpui::transparent_black())
    }
}

/// The default base color: a warm rose palette with a pink accent.
const AGENT_DESK: ThemeDef = ThemeDef {
    key: "agent-desk",
    title: "Agent Desk",
    light: &[
        ("background", 0xfdf9faff),
        ("foreground", 0x1f1a1cff),
        ("popover", 0xffffffff),
        ("popover-foreground", 0x1f1a1cff),
        ("primary", 0xd6336cff),
        ("primary-foreground", 0xffffffff),
        ("secondary", 0xf3eaedff),
        ("secondary-foreground", 0x1f1a1cff),
        ("muted", 0xf3eaedff),
        ("muted-foreground", 0x8a7f83ff),
        ("accent", 0xf3e8ecff),
        ("accent-foreground", 0x1f1a1cff),
        ("destructive", 0xe7000bff),
        ("border", 0xefe4e7ff),
        ("input", 0xe9dde1ff),
        ("ring", 0xd6336cff),
        ("sidebar", 0xf6ecefff),
        ("sidebar-foreground", 0x1f1a1cff),
        ("sidebar-primary", 0xd6336cff),
        ("sidebar-primary-foreground", 0xffffffff),
        ("sidebar-accent", 0xeedfe4ff),
        ("sidebar-accent-foreground", 0x1f1a1cff),
        ("sidebar-border", 0xe9dadfff),
    ],
    dark: &[
        ("background", 0x1c1719ff),
        ("foreground", 0xf3eceeff),
        ("popover", 0x241c1fff),
        ("popover-foreground", 0xf3eceeff),
        ("primary", 0xf06595ff),
        ("primary-foreground", 0xffffffff),
        ("secondary", 0x2e2528ff),
        ("secondary-foreground", 0xf3eceeff),
        ("muted", 0x2e2528ff),
        ("muted-foreground", 0xa0959aff),
        ("accent", 0x2e2528ff),
        ("accent-foreground", 0xf3eceeff),
        ("destructive", 0xff6467ff),
        ("border", 0x33292dff),
        ("input", 0x3a2f33ff),
        ("ring", 0xf06595ff),
        ("sidebar", 0x241c1fff),
        ("sidebar-foreground", 0xf3eceeff),
        ("sidebar-primary", 0xf06595ff),
        ("sidebar-primary-foreground", 0xffffffff),
        ("sidebar-accent", 0x33292dff),
        ("sidebar-accent-foreground", 0xf3eceeff),
        ("sidebar-border", 0x3a2f33ff),
    ],
};

/// All the base colors: Agent Desk first, then the shadcn/ui base colors.
pub fn bases() -> impl Iterator<Item = &'static ThemeDef> {
    std::iter::once(&AGENT_DESK).chain(shadcn_themes::BASES)
}

pub fn accents() -> impl Iterator<Item = &'static ThemeDef> {
    shadcn_themes::ACCENTS.iter()
}

/// A named pair of a base color and an accent color that look good together.
pub struct Preset {
    pub title: &'static str,
    pub base: &'static str,
    /// `None` uses the primary color of the base color.
    pub accent: Option<&'static str>,
}

/// The themes at the top of the theme menu. Each accent matches the tint of its base color.
pub const PRESETS: &[Preset] = &[
    Preset { title: "Agent Desk", base: "agent-desk", accent: None },
    Preset { title: "Graphite", base: "neutral", accent: None },
    Preset { title: "Cobalt", base: "zinc", accent: Some("blue") },
    Preset { title: "Midnight", base: "zinc", accent: Some("indigo") },
    Preset { title: "Lavender", base: "mauve", accent: Some("violet") },
    Preset { title: "Berry", base: "mauve", accent: Some("fuchsia") },
    Preset { title: "Ocean", base: "mist", accent: Some("sky") },
    Preset { title: "Lagoon", base: "mist", accent: Some("teal") },
    Preset { title: "Forest", base: "olive", accent: Some("green") },
    Preset { title: "Meadow", base: "olive", accent: Some("lime") },
    Preset { title: "Sandstone", base: "stone", accent: Some("amber") },
    Preset { title: "Rosewood", base: "stone", accent: Some("rose") },
    Preset { title: "Terracotta", base: "taupe", accent: Some("orange") },
];

impl Preset {
    /// The two colors that show this preset in a menu: the tint of the base, and the accent.
    pub fn swatches(&self) -> (Hsla, Hsla) {
        let base = bases().find(|t| t.key == self.base).unwrap_or(&AGENT_DESK);
        let accent = self
            .accent
            .and_then(|k| accents().find(|t| t.key == k))
            .map_or_else(|| base.swatch("primary"), |t| t.swatch("primary"));
        (base.swatch("muted-foreground"), accent)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    fn key(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// The theme that the user picked. The app keeps it in a file.
#[derive(Clone)]
pub struct ThemeChoice {
    pub base: &'static str,
    /// `None` uses the primary color of the base color.
    pub accent: Option<&'static str>,
    pub appearance: Appearance,
    file: PathBuf,
}

impl Global for ThemeChoice {}

impl ThemeChoice {
    /// Read the choice from `file`, as `key=value` lines. A missing or bad value gets the default.
    pub fn load(file: PathBuf) -> Self {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let get = |name: &str| {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == name)
                .map(|(_, v)| v.trim().to_string())
        };
        let base = get("base")
            .and_then(|k| bases().find(|t| t.key == k))
            .unwrap_or(&AGENT_DESK)
            .key;
        let accent = get("accent").and_then(|k| accents().find(|t| t.key == k)).map(|t| t.key);
        let appearance = get("appearance")
            .and_then(|k| Appearance::ALL.into_iter().find(|a| a.key() == k))
            .unwrap_or(Appearance::System);
        Self {
            base,
            accent,
            appearance,
            file,
        }
    }

    /// The preset with the same base color and accent color, if there is one.
    pub fn preset(&self) -> Option<&'static Preset> {
        PRESETS.iter().find(|p| p.base == self.base && p.accent == self.accent)
    }

    fn save(&self) {
        let text = format!(
            "base={}\naccent={}\nappearance={}\n",
            self.base,
            self.accent.unwrap_or(""),
            self.appearance.key()
        );
        if let Err(e) = std::fs::write(&self.file, text) {
            tracing::warn!("cannot save the theme: {e}");
        }
    }
}

fn lookup(vars: &[(&str, u32)], var: &str) -> Option<Hsla> {
    vars.iter().rev().find(|(k, _)| *k == var).map(|&(_, v)| rgba(v).into())
}

/// Change the theme, keep it, and show it.
pub fn set(window: Option<&mut Window>, cx: &mut App, edit: impl FnOnce(&mut ThemeChoice)) {
    let choice = cx.global_mut::<ThemeChoice>();
    edit(choice);
    choice.save();
    apply(window, cx);
}

/// Show the chosen theme. Call this again when the system appearance changes.
pub fn apply(window: Option<&mut Window>, cx: &mut App) {
    let choice = cx.global::<ThemeChoice>().clone();
    match choice.appearance {
        Appearance::System => Theme::sync_system_appearance(window, cx),
        Appearance::Light => Theme::change(ThemeMode::Light, window, cx),
        Appearance::Dark => Theme::change(ThemeMode::Dark, window, cx),
    }
    let dark = Theme::global(cx).is_dark();
    let base = bases().find(|t| t.key == choice.base).unwrap_or(&AGENT_DESK);
    let accent = choice.accent.and_then(|k| accents().find(|t| t.key == k));
    // The accent colors go on top of the base colors. `lookup` finds the last value.
    let vars: Vec<(&str, u32)> = base
        .vars(dark)
        .iter()
        .chain(accent.map(|a| a.vars(dark)).unwrap_or_default())
        .copied()
        .collect();
    let var = |name: &str| lookup(&vars, name);

    Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        let set = |field: &mut Hsla, name: &str| {
            if let Some(color) = var(name) {
                *field = color;
            }
        };
        set(&mut c.background, "background");
        set(&mut c.foreground, "foreground");
        set(&mut c.popover, "popover");
        set(&mut c.popover_foreground, "popover-foreground");
        set(&mut c.secondary, "secondary");
        set(&mut c.secondary_foreground, "secondary-foreground");
        set(&mut c.muted, "muted");
        set(&mut c.muted_foreground, "muted-foreground");
        set(&mut c.accent, "accent");
        set(&mut c.accent_foreground, "accent-foreground");
        set(&mut c.danger, "destructive");
        set(&mut c.border, "border");
        set(&mut c.input, "input");
        set(&mut c.ring, "ring");
        set(&mut c.sidebar, "sidebar");
        set(&mut c.sidebar_foreground, "sidebar-foreground");
        set(&mut c.sidebar_primary, "sidebar-primary");
        set(&mut c.sidebar_primary_foreground, "sidebar-primary-foreground");
        set(&mut c.sidebar_accent, "sidebar-accent");
        set(&mut c.sidebar_accent_foreground, "sidebar-accent-foreground");
        set(&mut c.sidebar_border, "sidebar-border");
        // In dark mode, the shadcn accent colors have a dark `primary`, made for buttons. It shows
        // badly as text on a dark background. Their `sidebar-primary` pair is brighter, so use it.
        let (primary_var, on_primary_var) = if dark && accent.is_some() {
            ("sidebar-primary", "sidebar-primary-foreground")
        } else {
            ("primary", "primary-foreground")
        };
        if let (Some(primary), Some(on_primary)) = (var(primary_var), var(on_primary_var)) {
            c.primary = primary;
            c.primary_hover = primary.opacity(0.9);
            c.primary_active = primary.opacity(0.8);
            c.primary_foreground = on_primary;
            c.button_primary = primary;
            c.button_primary_hover = primary.opacity(0.9);
            c.button_primary_active = primary.opacity(0.8);
            c.button_primary_foreground = on_primary;
            c.caret = primary;
            c.link = primary;
            c.selection = primary.opacity(0.25);
            c.progress_bar = primary;
        }
    });
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_name_known_colors() {
        for p in PRESETS {
            assert!(bases().any(|t| t.key == p.base), "{}: no base color {}", p.title, p.base);
            if let Some(a) = p.accent {
                assert!(accents().any(|t| t.key == a), "{}: no accent color {a}", p.title);
            }
        }
    }
}
