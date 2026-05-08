use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use smelt_term::grid::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Lang(pub &'static str);

struct Def {
    name: &'static str,
    color: (u8, u8, u8),
    exts: &'static [&'static str],
    filenames: &'static [&'static str],
}

const fn rgb(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    (r, g, b)
}

/// One canonical entry per language. Add new languages here and both
/// detection and color lookup pick them up automatically.
static LANGS: &[Def] = &[
    Def {
        name: "Rust",
        color: rgb(222, 165, 132),
        exts: &["rs"],
        filenames: &[],
    },
    Def {
        name: "Go",
        color: rgb(0, 173, 216),
        exts: &["go"],
        filenames: &[],
    },
    Def {
        name: "Python",
        color: rgb(53, 114, 165),
        exts: &["py", "pyi"],
        filenames: &[],
    },
    Def {
        name: "JavaScript",
        color: rgb(241, 224, 90),
        exts: &["js", "mjs", "cjs"],
        filenames: &[],
    },
    Def {
        name: "JSX",
        color: rgb(255, 200, 90),
        exts: &["jsx"],
        filenames: &[],
    },
    Def {
        name: "TypeScript",
        color: rgb(49, 120, 198),
        exts: &["ts"],
        filenames: &[],
    },
    Def {
        name: "TSX",
        color: rgb(99, 160, 230),
        exts: &["tsx"],
        filenames: &[],
    },
    Def {
        name: "C",
        color: rgb(85, 85, 85),
        exts: &["c", "h"],
        filenames: &[],
    },
    Def {
        name: "C++",
        color: rgb(243, 75, 125),
        exts: &["cc", "cpp", "cxx", "hpp", "hh", "hxx"],
        filenames: &[],
    },
    Def {
        name: "C#",
        color: rgb(23, 134, 0),
        exts: &["cs"],
        filenames: &[],
    },
    Def {
        name: "Java",
        color: rgb(176, 114, 25),
        exts: &["java"],
        filenames: &[],
    },
    Def {
        name: "Kotlin",
        color: rgb(169, 123, 255),
        exts: &["kt", "kts"],
        filenames: &[],
    },
    Def {
        name: "Swift",
        color: rgb(255, 172, 69),
        exts: &["swift"],
        filenames: &[],
    },
    Def {
        name: "Ruby",
        color: rgb(112, 21, 22),
        exts: &["rb"],
        filenames: &["Rakefile", "Gemfile"],
    },
    Def {
        name: "PHP",
        color: rgb(79, 93, 149),
        exts: &["php"],
        filenames: &[],
    },
    Def {
        name: "Lua",
        color: rgb(0, 0, 128),
        exts: &["lua"],
        filenames: &[],
    },
    Def {
        name: "Perl",
        color: rgb(150, 150, 150),
        exts: &["pl", "pm"],
        filenames: &[],
    },
    Def {
        name: "Shell",
        color: rgb(137, 224, 81),
        exts: &["sh", "bash", "zsh"],
        filenames: &[],
    },
    Def {
        name: "Fish",
        color: rgb(74, 174, 71),
        exts: &["fish"],
        filenames: &[],
    },
    Def {
        name: "HTML",
        color: rgb(227, 76, 38),
        exts: &["html", "htm"],
        filenames: &[],
    },
    Def {
        name: "CSS",
        color: rgb(86, 61, 124),
        exts: &["css"],
        filenames: &[],
    },
    Def {
        name: "Sass",
        color: rgb(204, 102, 153),
        exts: &["scss", "sass"],
        filenames: &[],
    },
    Def {
        name: "Less",
        color: rgb(29, 54, 93),
        exts: &["less"],
        filenames: &[],
    },
    Def {
        name: "JSON",
        color: rgb(64, 64, 64),
        exts: &["json"],
        filenames: &[],
    },
    Def {
        name: "YAML",
        color: rgb(203, 23, 30),
        exts: &["yaml", "yml"],
        filenames: &[],
    },
    Def {
        name: "TOML",
        color: rgb(156, 66, 33),
        exts: &["toml"],
        filenames: &["Cargo.toml", "Cargo.lock"],
    },
    Def {
        name: "XML",
        color: rgb(0, 96, 172),
        exts: &["xml"],
        filenames: &[],
    },
    Def {
        name: "Markdown",
        color: rgb(8, 63, 161),
        exts: &["md", "markdown"],
        filenames: &[],
    },
    Def {
        name: "SQL",
        color: rgb(225, 134, 24),
        exts: &["sql"],
        filenames: &[],
    },
    Def {
        name: "Vim",
        color: rgb(25, 159, 75),
        exts: &["vim"],
        filenames: &[],
    },
    Def {
        name: "Elisp",
        color: rgb(192, 101, 219),
        exts: &["el"],
        filenames: &[],
    },
    Def {
        name: "Elixir",
        color: rgb(110, 74, 126),
        exts: &["ex", "exs"],
        filenames: &[],
    },
    Def {
        name: "Erlang",
        color: rgb(184, 57, 152),
        exts: &["erl", "hrl"],
        filenames: &[],
    },
    Def {
        name: "Haskell",
        color: rgb(94, 80, 134),
        exts: &["hs"],
        filenames: &[],
    },
    Def {
        name: "OCaml",
        color: rgb(238, 106, 26),
        exts: &["ml", "mli"],
        filenames: &[],
    },
    Def {
        name: "Scala",
        color: rgb(194, 45, 64),
        exts: &["scala", "sc"],
        filenames: &[],
    },
    Def {
        name: "Clojure",
        color: rgb(219, 88, 85),
        exts: &["clj", "cljs", "cljc"],
        filenames: &[],
    },
    Def {
        name: "Dart",
        color: rgb(0, 180, 171),
        exts: &["dart"],
        filenames: &[],
    },
    Def {
        name: "R",
        color: rgb(25, 140, 232),
        exts: &["r"],
        filenames: &[],
    },
    Def {
        name: "Julia",
        color: rgb(162, 112, 186),
        exts: &["jl"],
        filenames: &[],
    },
    Def {
        name: "Nim",
        color: rgb(255, 200, 0),
        exts: &["nim"],
        filenames: &[],
    },
    Def {
        name: "Zig",
        color: rgb(236, 145, 92),
        exts: &["zig"],
        filenames: &[],
    },
    Def {
        name: "Vue",
        color: rgb(65, 184, 131),
        exts: &["vue"],
        filenames: &[],
    },
    Def {
        name: "Svelte",
        color: rgb(255, 62, 0),
        exts: &["svelte"],
        filenames: &[],
    },
    Def {
        name: "TeX",
        color: rgb(62, 116, 172),
        exts: &["tex"],
        filenames: &[],
    },
    Def {
        name: "Protobuf",
        color: rgb(187, 86, 92),
        exts: &["proto"],
        filenames: &[],
    },
    Def {
        name: "GraphQL",
        color: rgb(225, 0, 152),
        exts: &["graphql", "gql"],
        filenames: &[],
    },
    Def {
        name: "Terraform",
        color: rgb(98, 78, 197),
        exts: &["tf", "tfvars"],
        filenames: &[],
    },
    Def {
        name: "Nix",
        color: rgb(82, 124, 180),
        exts: &["nix"],
        filenames: &[],
    },
    Def {
        name: "Make",
        color: rgb(66, 124, 82),
        exts: &[],
        filenames: &["Makefile", "makefile", "GNUmakefile"],
    },
    Def {
        name: "Dockerfile",
        color: rgb(56, 79, 187),
        exts: &[],
        filenames: &["Dockerfile", "Containerfile"],
    },
    Def {
        name: "CMake",
        color: rgb(218, 49, 53),
        exts: &[],
        filenames: &["CMakeLists.txt"],
    },
];

struct Tables {
    by_ext: HashMap<&'static str, &'static Def>,
    by_name: HashMap<&'static str, &'static Def>,
    color_by_lang: HashMap<&'static str, Color>,
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut by_ext = HashMap::new();
        let mut by_name = HashMap::new();
        let mut color_by_lang = HashMap::new();
        for def in LANGS {
            let (r, g, b) = def.color;
            color_by_lang.insert(def.name, Color::Rgb { r, g, b });
            for ext in def.exts {
                by_ext.insert(*ext, def);
            }
            for name in def.filenames {
                by_name.insert(*name, def);
            }
        }
        Tables {
            by_ext,
            by_name,
            color_by_lang,
        }
    })
}

pub fn detect(path: &Path) -> Option<Lang> {
    let _g = smelt_perf::perf::begin("lang.detect");
    let t = tables();
    if let Some(name) = path.file_name().and_then(|s| s.to_str())
        && let Some(def) = t.by_name.get(name)
    {
        return Some(Lang(def.name));
    }
    let ext = path.extension()?.to_str()?;
    let bytes = ext.as_bytes();
    let mut buf = [0u8; 16];
    if bytes.len() <= buf.len() && bytes.iter().all(|b| b.is_ascii()) {
        for (i, b) in bytes.iter().enumerate() {
            buf[i] = b.to_ascii_lowercase();
        }
        let lower = std::str::from_utf8(&buf[..bytes.len()]).ok()?;
        return t.by_ext.get(lower).map(|def| Lang(def.name));
    }
    // Rare: oversized or non-ASCII extension. Allocate temporarily.
    let lower = ext.to_ascii_lowercase();
    t.by_ext.get(lower.as_str()).map(|def| Lang(def.name))
}

pub fn color(lang: Lang) -> Color {
    tables()
        .color_by_lang
        .get(lang.0)
        .copied()
        .unwrap_or(Color::Rgb {
            r: 150,
            g: 150,
            b: 150,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn detect_by_extension() {
        assert_eq!(detect(Path::new("foo.rs")), Some(Lang("Rust")));
        assert_eq!(detect(Path::new("dir/sub/main.go")), Some(Lang("Go")));
        assert_eq!(detect(Path::new("MAIN.RS")), Some(Lang("Rust")));
        assert_eq!(detect(Path::new("foo.unknown")), None);
        assert_eq!(detect(Path::new("noext")), None);
    }

    #[test]
    fn detect_by_filename() {
        assert_eq!(detect(Path::new("Makefile")), Some(Lang("Make")));
        assert_eq!(
            detect(Path::new("path/Dockerfile")),
            Some(Lang("Dockerfile"))
        );
        assert_eq!(detect(Path::new("Cargo.toml")), Some(Lang("TOML")));
        assert_eq!(detect(Path::new("Cargo.lock")), Some(Lang("TOML")));
    }

    #[test]
    fn color_round_trip() {
        let r = detect(Path::new("a.rs")).unwrap();
        // Just ensure it returns a defined RGB color.
        match color(r) {
            Color::Rgb { .. } => {}
            _ => panic!("expected Rgb"),
        }
    }
}
