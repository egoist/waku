use std::borrow::Cow;

use anyhow::Result;
use gpui::{App, AssetSource, SharedString};

/// Icons embedded in the binary so the app stays a single artifact.
pub struct Assets;

macro_rules! icons {
    ($($name:literal),+ $(,)?) => {
        &[$((
            concat!("icons/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/", $name, ".svg")).as_slice(),
        )),+]
    };
}

const ICONS: &[(&str, &[u8])] = icons![
    "alert",
    "appearance",
    "arrow-left",
    "arrow-right",
    "arrow-up",
    "block",
    "check",
    "chevron-down",
    "chevron-right",
    "copy",
    "cursor-spark",
    "folder",
    "folder-new",
    "file-diff",
    "file-types/angular",
    "file-types/audio",
    "file-types/astro",
    "file-types/babel",
    "file-types/biome",
    "file-types/bun",
    "file-types/c",
    "file-types/certificate",
    "file-types/clojure",
    "file-types/cmake",
    "file-types/coffee",
    "file-types/console",
    "file-types/cpp",
    "file-types/crystal",
    "file-types/csharp",
    "file-types/css",
    "file-types/dart",
    "file-types/database",
    "file-types/deno",
    "file-types/diff",
    "file-types/docker",
    "file-types/editorconfig",
    "file-types/elixir",
    "file-types/elm",
    "file-types/erlang",
    "file-types/eslint",
    "file-types/exe",
    "file-types/file",
    "file-types/firebase",
    "file-types/git",
    "file-types/gitlab",
    "file-types/go",
    "file-types/gradle",
    "file-types/graphql",
    "file-types/haskell",
    "file-types/haxe",
    "file-types/helm",
    "file-types/html",
    "file-types/image",
    "file-types/java",
    "file-types/javascript",
    "file-types/jinja",
    "file-types/json",
    "file-types/julia",
    "file-types/kotlin",
    "file-types/kubernetes",
    "file-types/lock",
    "file-types/lua",
    "file-types/makefile",
    "file-types/markdown",
    "file-types/nest",
    "file-types/next",
    "file-types/nginx",
    "file-types/nix",
    "file-types/nodejs",
    "file-types/npm",
    "file-types/nuxt",
    "file-types/ocaml",
    "file-types/pdf",
    "file-types/perl",
    "file-types/php",
    "file-types/pnpm",
    "file-types/powershell",
    "file-types/prettier",
    "file-types/prisma",
    "file-types/proto",
    "file-types/pug",
    "file-types/python",
    "file-types/react",
    "file-types/readme",
    "file-types/rollup",
    "file-types/ruby",
    "file-types/rust",
    "file-types/sass",
    "file-types/scala",
    "file-types/settings",
    "file-types/solidity",
    "file-types/storybook",
    "file-types/stylelint",
    "file-types/supabase",
    "file-types/svelte",
    "file-types/svg",
    "file-types/swift",
    "file-types/tailwindcss",
    "file-types/terraform",
    "file-types/tex",
    "file-types/turborepo",
    "file-types/typescript",
    "file-types/video",
    "file-types/vite",
    "file-types/vitest",
    "file-types/vue",
    "file-types/webassembly",
    "file-types/webpack",
    "file-types/xaml",
    "file-types/xml",
    "file-types/yaml",
    "file-types/yarn",
    "file-types/zig",
    "file-types/zip",
    "fork",
    "git-branch",
    "globe",
    "hexagon",
    "list",
    "lock",
    "lock-open",
    "panel-left",
    "panel-right",
    "pencil",
    "plus",
    "provider-amp",
    "provider-claude",
    "provider-cursor",
    "provider-grok",
    "provider-openai",
    "provider-opencode",
    "provider-pi",
    "rewind",
    "search",
    "settings",
    "slash",
    "sparkle",
    "star",
    "star-filled",
    "stop",
    "terminal",
    "wrench",
    "x",
    "zap",
];

const FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf"),
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf"),
];

pub fn register_fonts(cx: &App) -> Result<()> {
    cx.text_system().add_fonts(
        FONTS
            .iter()
            .map(|font| Cow::Borrowed(*font))
            .collect::<Vec<_>>(),
    )
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
