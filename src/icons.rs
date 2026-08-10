//! Tray icons built from the Syncthing mark.
//!
//! Two styles are produced from one SVG template:
//!
//! * **Colour** — the mark on its blue disc, with a coloured state emblem. Sent
//!   as pixmaps, because no icon theme ships a Syncthing status icon, so naming
//!   one would risk an empty tray slot.
//! * **Monochrome** — the mark alone in a single colour, for panels styled with
//!   symbolic icons. These are written to disk as a small `hicolor` theme and
//!   referenced by name, because recolouring to match the panel happens in
//!   Plasma's icon engine and only applies to icons it loads from a theme. A
//!   pixmap would be stuck with whatever colour we baked into it.
//!
//! In both styles the state is carried by an emblem shape as well as by colour,
//! so the two are distinguishable without relying on hue.

use crate::model::Health;
use anyhow::{Context, Result};
use ksni::Icon;
use std::path::PathBuf;
use std::sync::LazyLock;

const TEMPLATE: &str = include_str!("../resources/icons/syncthing.svg");

/// Sizes offered to the host, which picks whichever suits its panel.
const SIZES: [u32; 5] = [16, 22, 24, 32, 48];

/// Syncthing's own blue, averaged from the gradient in its logo.
const SYNCTHING_BLUE: &str = "#1799d1";

/// How the tray icon should be drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// A single colour, recoloured by the panel. The default, because Plasma's
    /// tray styles status icons symbolically and a colour icon looks foreign
    /// beside the rest of the panel.
    #[default]
    Monochrome,
    Colour,
}

impl Style {
    /// Read the preferred style from the environment.
    ///
    /// There is no way to detect this: the StatusNotifierItem protocol does not
    /// describe the panel's styling, and the host never tells the item whether
    /// it is being drawn into a monochrome tray. So it is a setting.
    pub fn from_env() -> Self {
        match std::env::var("SYNCERTING_ICON_STYLE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "colour" | "color" | "full" => Style::Colour,
            _ => Style::Monochrome,
        }
    }
}

/// Colour of the state emblem in the colour style.
fn emblem_colour(health: Health) -> &'static str {
    match health {
        Health::Ok => "#3ea652",
        // The emblem sits in a gap punched through the mark, so it is drawn
        // against the panel, not against the disc. White would disappear on a
        // light panel, so the activity dot takes the mark's own blue.
        Health::Syncing | Health::Starting => SYNCTHING_BLUE,
        Health::Error => "#d64545",
        Health::Paused => "#f0a02b",
        Health::Stopped => "#ffffff",
    }
}

/// The emblem marking each state, drawn over a gap punched in the mark.
///
/// `Ok` has none: an unadorned Syncthing logo is the resting state.
fn emblem(health: Health, fill: &str) -> String {
    match health {
        Health::Ok => String::new(),
        // A single dot reads as activity at 16px, where arrows turn to mush.
        Health::Syncing | Health::Starting => {
            format!(r#"<circle cx="92" cy="92" r="16" fill="{fill}"/>"#)
        }
        Health::Paused => format!(
            r#"<g fill="{fill}"><rect x="79" y="76" width="10" height="32" rx="4"/><rect x="95" y="76" width="10" height="32" rx="4"/></g>"#
        ),
        Health::Error => format!(
            r#"<g fill="{fill}"><rect x="86" y="70" width="12" height="28" rx="6"/><circle cx="92" cy="106" r="7"/></g>"#
        ),
        // Dimmed rather than badged; "not running" is the absence of activity.
        Health::Stopped => String::new(),
    }
}

/// Whole-icon opacity. Only the stopped state is dimmed.
fn opacity(health: Health) -> &'static str {
    match health {
        Health::Stopped => "0.55",
        _ => "1",
    }
}

/// Build the SVG for one state in one style.
fn svg_for(health: Health, style: Style) -> String {
    let has_emblem = !matches!(health, Health::Ok | Health::Stopped);

    // The gap keeps the emblem legible where it would otherwise sit on top of
    // the mark's lower-right node.
    let (defs, mask) = if has_emblem {
        (
            r##"<defs><mask id="gap"><rect x="0" y="0" width="117.3" height="117.3" fill="#fff"/><circle cx="92" cy="92" r="30" fill="#000"/></mask></defs>"##,
            r#" mask="url(#gap)""#,
        )
    } else {
        ("", "")
    };

    let (style_block, class, disc, mark, emblem_fill) = match style {
        Style::Colour => ("", "", SYNCTHING_BLUE, "#ffffff", emblem_colour(health)),
        // currentColor plus the Breeze colour-scheme class is what lets Plasma
        // repaint the icon in the panel's foreground colour.
        Style::Monochrome => (
            r##"<defs><style type="text/css" id="current-color-scheme">.ColorScheme-Text { color:#232629; }</style></defs>"##,
            r#" class="ColorScheme-Text" fill="currentColor""#,
            "none",
            "currentColor",
            "currentColor",
        ),
    };

    TEMPLATE
        .replace("{{STYLE}}", style_block)
        .replace("{{DEFS}}", defs)
        .replace("{{MASK}}", mask)
        .replace("{{CLASS}}", class)
        .replace("{{OPACITY}}", opacity(health))
        .replace("{{DISC}}", disc)
        .replace("{{MARK}}", mark)
        .replace("{{BADGE}}", &emblem(health, emblem_fill))
}

/// Rasterise `svg` to a square ARGB32 pixmap of `size` pixels.
fn render(svg: &str, size: u32) -> Option<Icon> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;

    let scale = size as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    // tiny-skia stores premultiplied RGBA; the tray expects straight ARGB in
    // network byte order, so each pixel is demultiplied and reordered.
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for pixel in pixmap.pixels() {
        let colour = pixel.demultiply();
        data.push(colour.alpha());
        data.push(colour.red());
        data.push(colour.green());
        data.push(colour.blue());
    }

    Some(Icon {
        width: size as i32,
        height: size as i32,
        data,
    })
}

/// Colour pixmaps for every state, rendered once on first use.
static COLOUR_ICONS: LazyLock<Vec<(Health, Vec<Icon>)>> = LazyLock::new(|| {
    Health::ALL
        .iter()
        .map(|&health| {
            let svg = svg_for(health, Style::Colour);
            let rendered = SIZES
                .iter()
                .filter_map(|&size| render(&svg, size))
                .collect();
            (health, rendered)
        })
        .collect()
});

/// Pixmaps for `health`. Empty in the monochrome style, where the icon is named
/// instead so that Plasma can recolour it.
pub fn for_health(health: Health, style: Style) -> Vec<Icon> {
    if style == Style::Monochrome {
        return Vec::new();
    }
    COLOUR_ICONS
        .iter()
        .find(|(candidate, _)| *candidate == health)
        .map(|(_, icons)| icons.clone())
        .unwrap_or_default()
}

/// Whether usable artwork exists for `health`, without cloning the pixmaps.
pub fn available(health: Health, style: Style) -> bool {
    style == Style::Colour
        && COLOUR_ICONS
            .iter()
            .any(|(candidate, icons)| *candidate == health && !icons.is_empty())
}

/// Themed icon name for `health`, used in the monochrome style.
pub fn icon_name(health: Health) -> String {
    let state = match health {
        Health::Ok => "ok",
        Health::Syncing => "syncing",
        Health::Starting => "starting",
        Health::Error => "error",
        Health::Paused => "paused",
        Health::Stopped => "stopped",
    };
    format!("syncerting-tray-{state}")
}

/// Where the generated monochrome theme is written.
fn theme_root() -> Result<PathBuf> {
    let cache = dirs::cache_dir().context("cannot determine the user cache directory")?;
    Ok(cache.join("syncerting-tray/icons"))
}

/// Write the monochrome icons as a minimal `hicolor` theme and return its root.
///
/// It has to be `hicolor` specifically: adding a search path does not change
/// which theme Plasma looks in, and hicolor is the fallback every theme
/// inherits, so icons placed there are always found.
pub fn install_monochrome_theme() -> Result<PathBuf> {
    install_monochrome_theme_at(theme_root()?)
}

/// As [`install_monochrome_theme`], but rooted at `root` so it can be tested
/// without writing into the real cache directory.
fn install_monochrome_theme_at(root: PathBuf) -> Result<PathBuf> {
    let dir = root.join("hicolor/scalable/status");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    std::fs::write(
        root.join("hicolor/index.theme"),
        "[Icon Theme]\n\
         Name=hicolor\n\
         Comment=Fallback icon theme\n\
         Directories=scalable/status\n\
         \n\
         [scalable/status]\n\
         Size=22\n\
         MinSize=8\n\
         MaxSize=512\n\
         Type=Scalable\n\
         Context=Status\n",
    )
    .context("writing index.theme")?;

    for health in Health::ALL {
        let path = dir.join(format!("{}.svg", icon_name(health)));
        std::fs::write(&path, svg_for(health, Style::Monochrome))
            .with_context(|| format!("writing {}", path.display()))?;
    }

    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_leaves_no_placeholders_behind() {
        for health in Health::ALL {
            for style in [Style::Colour, Style::Monochrome] {
                let svg = svg_for(health, style);
                assert!(
                    !svg.contains("{{"),
                    "unsubstituted placeholder for {health:?} in {style:?}"
                );
            }
        }
    }

    #[test]
    fn every_state_renders_at_every_size() {
        for health in Health::ALL {
            let icons = for_health(health, Style::Colour);
            assert_eq!(icons.len(), SIZES.len(), "missing sizes for {health:?}");

            for icon in icons {
                // ARGB32: four bytes per pixel, exactly width * height of them.
                assert_eq!(icon.width, icon.height);
                assert_eq!(
                    icon.data.len(),
                    (icon.width * icon.height * 4) as usize,
                    "wrong buffer length for {health:?} at {}px",
                    icon.width
                );
            }
        }
    }

    #[test]
    fn the_artwork_is_actually_drawn() {
        // A fully transparent pixmap would mean a blank tray slot, which is the
        // failure this whole module exists to avoid.
        for health in Health::ALL {
            let icon = for_health(health, Style::Colour)
                .into_iter()
                .find(|i| i.width == 22)
                .expect("22px icon");
            let opaque = icon.data.chunks_exact(4).filter(|px| px[0] > 0).count();
            assert!(opaque > 20, "{health:?} rendered almost nothing");
        }
    }

    #[test]
    fn states_are_visually_distinct() {
        // Two states that rendered identically would be indistinguishable in the
        // panel however correct the rest of the logic is.
        let at_22 = |health: Health| {
            for_health(health, Style::Colour)
                .into_iter()
                .find(|i| i.width == 22)
                .expect("22px icon")
                .data
        };

        let states = [
            Health::Ok,
            Health::Syncing,
            Health::Error,
            Health::Paused,
            Health::Stopped,
        ];
        for (i, a) in states.iter().enumerate() {
            for b in &states[i + 1..] {
                assert_ne!(at_22(*a), at_22(*b), "{a:?} and {b:?} look identical");
            }
        }
    }

    #[test]
    fn monochrome_states_differ_by_shape_not_only_colour() {
        // The monochrome style has no colour to spend, so the emblems must carry
        // the difference on their own.
        let render_mono = |health: Health| {
            let svg = svg_for(health, Style::Monochrome)
                // currentColor has no meaning outside a themed context, so pin
                // it to something concrete purely to compare shapes.
                .replace("currentColor", "#000000");
            render(&svg, 32).expect("renders").data
        };

        let states = [
            Health::Ok,
            Health::Syncing,
            Health::Error,
            Health::Paused,
            Health::Stopped,
        ];
        for (i, a) in states.iter().enumerate() {
            for b in &states[i + 1..] {
                assert_ne!(render_mono(*a), render_mono(*b), "{a:?} matches {b:?}");
            }
        }
    }

    #[test]
    fn the_monochrome_style_carries_the_recolour_hooks() {
        let svg = svg_for(Health::Ok, Style::Monochrome);
        // Both are required for Plasma to repaint the icon to match the panel.
        assert!(svg.contains("ColorScheme-Text"));
        assert!(svg.contains("currentColor"));
        // A filled disc would defeat a symbolic icon.
        assert!(svg.contains(r##"fill="none""##));
    }

    #[test]
    fn the_default_style_is_monochrome() {
        // Plasma's tray is symbolic, so this is the one that looks native.
        assert_eq!(Style::default(), Style::Monochrome);
    }

    #[test]
    fn the_monochrome_theme_is_written_where_plasma_will_find_it() {
        let root = std::env::temp_dir().join(format!("syncerting-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        install_monochrome_theme_at(root.clone()).expect("writes the theme");

        // hicolor specifically: every theme inherits it, so icons placed there
        // are always found.
        let index = std::fs::read_to_string(root.join("hicolor/index.theme")).unwrap();
        assert!(index.contains("Name=hicolor"));

        for health in Health::ALL {
            let svg = root
                .join("hicolor/scalable/status")
                .join(format!("{}.svg", icon_name(health)));
            let text = std::fs::read_to_string(&svg)
                .unwrap_or_else(|_| panic!("missing {}", svg.display()));
            assert!(
                text.contains("currentColor"),
                "{health:?} is not recolourable"
            );
            // Parsing it is the check that it is usable artwork, not just a file.
            resvg::usvg::Tree::from_str(&text, &resvg::usvg::Options::default())
                .unwrap_or_else(|e| panic!("{health:?} is not valid SVG: {e}"));
        }

        std::fs::remove_dir_all(&root).ok();
    }
}
