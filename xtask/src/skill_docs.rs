//! Check the Markdown conventions used by the unlit3d skill pages.

use std::fs;
use std::path::Path;

const PAGES: &[&str] = &[
    "SKILL.md",
    "SCENE.md",
    "MULTI_WORLD.md",
    "MCP.md",
    "EXAMPLES.md",
    "TROUBLESHOOTING.md",
];

pub(crate) fn check(root: &Path) -> Result<(), String> {
    let dir = root.join(".agents/skills/unlit3d");
    let mut errors = Vec::new();
    for name in PAGES {
        let path = dir.join(name);
        match fs::read_to_string(&path) {
            Ok(text) => errors.extend(
                lint(&text, &dir)
                    .into_iter()
                    .map(|(line, issue)| format!("{}:{line}: {issue}", path.display())),
            ),
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    println!("Checked {} unlit3d skill pages", PAGES.len());
    Ok(())
}

fn lint(text: &str, dir: &Path) -> Vec<(usize, String)> {
    let mut errors = Vec::new();
    let mut level = 0;
    let mut open = None;
    let mut previous = "";
    let mut columns = None;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let mut report = |message: &str| errors.push((line, message.to_owned()));
        if raw.ends_with([' ', '\t']) {
            report("trailing whitespace");
        }
        let content = raw.trim();
        if content.is_empty() {
            columns = None;
            continue;
        }
        if let Some(language) = content.strip_prefix("```") {
            if open.is_some() {
                if !language.is_empty() {
                    report("closing fence must be bare");
                }
                open = None;
            } else {
                open = Some(line);
                if language.is_empty() || language.contains(char::is_whitespace) {
                    report("opening fence requires a language");
                }
                if !["可编译骨架", "示意片段", "命令", "输出示例"]
                    .iter()
                    .any(|label| previous.starts_with(&format!("**{label}**：")))
                {
                    report("classify the fence on the preceding nonempty line");
                }
                if previous.starts_with("**可编译骨架**：")
                    && !(previous.contains("](") && previous.contains("`cargo "))
                {
                    report(
                        "a compilable fence needs a linked source and a Cargo verification command",
                    );
                }
            }
            previous = content;
            continue;
        }
        if open.is_some() {
            continue;
        }
        if let Some((depth, _)) = heading(content) {
            if depth == 1 && level != 0 {
                report("duplicate top-level heading");
            }
            if depth > level + 1 || depth > 6 {
                report("skipped heading level");
            }
            level = depth;
        }
        if content.starts_with('|') && content.ends_with('|') {
            let mut escaped = false;
            let mut code = false;
            let mut separators = 0;
            for ch in content.chars() {
                if !escaped {
                    if ch == '`' {
                        code = !code;
                    } else if ch == '|' && !code {
                        separators += 1;
                    }
                }
                escaped = !escaped && ch == '\\';
            }
            if *columns.get_or_insert(separators) != separators {
                report("table row has a different number of columns");
            }
        } else {
            columns = None;
        }
        for link in links(content) {
            if let Err(issue) = local_link(link, text, dir) {
                report(&issue);
            }
        }
        previous = content;
    }
    if let Some(line) = open {
        errors.push((line, "unclosed fence".to_owned()));
    }
    if level == 0 {
        errors.push((1, "missing top-level heading".to_owned()));
    }
    errors
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let depth = line.bytes().take_while(|&byte| byte == b'#').count();
    Some((depth, line.get(depth..)?.strip_prefix(' ')?)).filter(|(depth, _)| *depth != 0)
}

fn links(line: &str) -> impl Iterator<Item = &str> {
    line.split("](").skip(1).filter_map(|rest| {
        let target = rest.split_once(')')?.0;
        Some(
            target
                .strip_prefix('<')
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or(target),
        )
    })
}

fn local_link(link: &str, text: &str, dir: &Path) -> Result<(), String> {
    let (file, fragment) = link.split_once('#').unwrap_or((link, ""));
    if is_line_anchor(fragment) {
        return Err(format!(
            "use a file link and a symbol instead of a line anchor: {link}"
        ));
    }
    if file.contains("://") || file.starts_with("mailto:") {
        return Ok(());
    }
    let path = dir.join(file);
    if !file.is_empty() && !path.exists() {
        return Err(format!("missing link target {link}"));
    }
    if fragment.is_empty() {
        return Ok(());
    }
    let target;
    let contents = if file.is_empty() {
        text
    } else {
        target = fs::read_to_string(&path).map_err(|error| format!("reading {link}: {error}"))?;
        &target
    };
    if contents.lines().filter_map(heading).any(|(_, title)| {
        title
            .to_lowercase()
            .chars()
            .filter_map(|ch| match ch {
                ch if ch.is_alphanumeric() || matches!(ch, '-' | '_') => Some(ch),
                ' ' => Some('-'),
                _ => None,
            })
            .eq(fragment.chars())
    }) || contents.contains(&format!("id=\"{fragment}\""))
    {
        return Ok(());
    }
    Err(format!("missing link anchor {link}"))
}

fn is_line_anchor(fragment: &str) -> bool {
    let start = fragment
        .split_once('-')
        .map_or(fragment, |(start, _)| start);
    start.strip_prefix('L').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::lint;
    use std::path::Path;

    #[test]
    fn checks_fences_headings_tables_links_and_whitespace() {
        let problems = lint(
            "# Title\n\n### Jump\n| A | B |\n| - |\n[x](#nope)  \n```\ntext\n```\n# Again\n",
            Path::new("."),
        );
        assert_eq!(problems.len(), 7, "{problems:?}");
    }

    #[test]
    fn accepts_local_anchors_and_classified_fences() {
        assert!(lint("# Title\n\n## 查找 API\n[x](#查找-api)\n| A | B |\n| - | - |\n| `a | b` | c |\n\n**命令**：检查\n\n```sh\ncargo xtask skill-docs\n```\n", Path::new(".")).is_empty());
    }

    #[test]
    fn rejects_line_anchors_even_if_the_lines_exist() {
        let problems = lint(
            "# Title\n[x](#L1)\n[x](#L1-L2)\n[x](https://example.org/example.rs#L1)\n",
            Path::new("."),
        );
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|(_, issue)| issue.contains("instead of a line anchor"))
        );
    }
}
