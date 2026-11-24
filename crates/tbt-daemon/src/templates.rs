pub const SERVICE: &str = include_str!("../../../packaging/systemd/tbt-daemon.service");
pub const TIMER: &str = include_str!("../../../packaging/systemd/tbt-daemon.timer");
pub const PLIST: &str = include_str!("../../../packaging/launchd/com.tokenburntrack.daemon.plist");
pub const TASK_XML: &str = include_str!("../../../packaging/windows/tbt-daemon-task.xml");
pub const DESKTOP: &str = include_str!("../../../packaging/linux/tbt-app.desktop");
pub const ICON_SVG: &str = include_str!("../../../packaging/icons/tbt-app.svg");

const SERVICE_EXEC: &str = "ExecStart=%h/.local/bin/tbt-daemon --once";
const SERVICE_RW: &str = "ReadWritePaths=%h/.local/share/tokenburntrack";
const PLIST_BIN: &str = "@BIN@";
const PLIST_LOG_DIR: &str = "@LOG_DIR@";
const PLIST_ONCE: &str = "        <string>--once</string>\n";
const TASK_COMMAND: &str = r"%LOCALAPPDATA%\Programs\TokenBurnTrack\tbt-daemon.exe";
const TASK_ARGUMENTS: &str = "<Arguments>--once</Arguments>";
const DESKTOP_EXEC: &str = "Exec=tbt-app\n";

fn fill(template: &str, placeholder: &str, value: &str) -> String {
    assert!(
        template.contains(placeholder),
        "template lacks the placeholder {placeholder:?}"
    );
    template.replace(placeholder, value)
}

fn once_args(args: &[String]) -> impl Iterator<Item = &str> {
    std::iter::once("--once").chain(args.iter().map(String::as_str))
}

pub fn systemd_service(bin: &str, args: &[String], data_dir: &str) -> String {
    let exec = std::iter::once(bin)
        .chain(once_args(args))
        .map(|w| systemd_word(w, true))
        .collect::<Vec<_>>()
        .join(" ");
    let text = fill(SERVICE, SERVICE_EXEC, &format!("ExecStart={exec}"));
    fill(
        &text,
        SERVICE_RW,
        &format!("ReadWritePaths={}", systemd_word(data_dir, false)),
    )
}

pub fn launchd_plist(bin: &str, args: &[String], log_dir: &str) -> String {
    let mut arg_lines = String::from(PLIST_ONCE);
    for a in args {
        arg_lines.push_str(&format!("        <string>{}</string>\n", xml_escape(a)));
    }
    let text = fill(PLIST, PLIST_ONCE, &arg_lines);
    let text = fill(&text, PLIST_BIN, &xml_escape(bin));
    fill(&text, PLIST_LOG_DIR, &xml_escape(log_dir))
}

pub fn task_xml(bin: &str, args: &[String]) -> String {
    let arguments = once_args(args)
        .map(windows_arg)
        .collect::<Vec<_>>()
        .join(" ");
    let text = fill(TASK_XML, TASK_COMMAND, &xml_escape(bin));
    fill(
        &text,
        TASK_ARGUMENTS,
        &format!("<Arguments>{}</Arguments>", xml_escape(&arguments)),
    )
}

pub fn desktop_entry(app: Option<&str>) -> String {
    match app {
        Some(app) => fill(
            DESKTOP,
            DESKTOP_EXEC,
            &format!("Exec={}\n", desktop_exec_arg(app)),
        ),
        None => DESKTOP.to_owned(),
    }
}

fn systemd_word(s: &str, exec: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    let mut quote = s.is_empty();
    for c in s.chars() {
        match c {
            '%' => out.push_str("%%"),
            '$' if exec => out.push_str("$$"),
            '\\' => out.push_str("\\\\"),
            '"' => {
                out.push_str("\\\"");
                quote = true;
            }
            c if c.is_whitespace() || c == '\'' || c == ';' => {
                out.push(c);
                quote = true;
            }
            c => out.push(c),
        }
    }
    if quote { format!("\"{out}\"") } else { out }
}

fn desktop_exec_arg(s: &str) -> String {
    const RESERVED: &str = " \t\"'\\><~|&;$*?#()`";
    let quote = s.chars().any(|c| RESERVED.contains(c));
    let mut out = String::with_capacity(s.len() + 2);
    if quote {
        out.push('"');
    }
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    if quote {
        out.push('"');
    }
    out.replace('\\', "\\\\")
}

fn windows_arg(s: &str) -> String {
    if !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c == '"') {
        return s.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in s.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
            }
        }
        if c != '\\' {
            out.push(c);
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_well_formed_xml(text: &str) {
        let mut stack: Vec<&str> = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find('<') {
            assert!(
                !rest[..start].contains('>'),
                "stray '>' in {:?}",
                &rest[..start]
            );
            let after = &rest[start..];
            let (end, tag) = if let Some(body) = after.strip_prefix("<!--") {
                let end = body.find("-->").expect("unterminated comment");
                (4 + end + 3, None)
            } else {
                let end = after.find('>').expect("unterminated tag") + 1;
                (end, Some(&after[1..end - 1]))
            };
            match tag {
                Some(t) if t.starts_with('?') => assert!(t.ends_with('?'), "bad PI {t}"),
                Some(t) if t.starts_with('!') => {}
                Some(t) if t.ends_with('/') => {}
                Some(t) if t.starts_with('/') => {
                    let open = stack.pop().unwrap_or_else(|| panic!("unexpected {t}"));
                    assert_eq!(open, &t[1..], "mismatched closing tag");
                }
                Some(t) => {
                    let name = t.split_whitespace().next().expect("empty tag");
                    stack.push(name);
                }
                None => {}
            }
            let text_part = &rest[..start];
            for entity in text_part.split('&').skip(1) {
                let name = entity.split(';').next().unwrap();
                assert!(
                    ["amp", "lt", "gt", "quot", "apos"].contains(&name),
                    "bad entity &{name}"
                );
            }
            rest = &after[end..];
        }
        assert!(stack.is_empty(), "unclosed tags {stack:?}");
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn templates_are_well_formed() {
        assert_well_formed_xml(PLIST);
        assert_well_formed_xml(TASK_XML);
        assert_well_formed_xml(ICON_SVG);
        assert!(SERVICE.contains(SERVICE_EXEC) && SERVICE.contains(SERVICE_RW));
        assert!(TIMER.contains("OnCalendar=hourly") && TIMER.contains("Persistent=true"));
        assert!(DESKTOP.contains(DESKTOP_EXEC) && DESKTOP.contains("StartupWMClass=tbt-app\n"));
    }

    #[test]
    fn service_with_plain_paths() {
        let unit = systemd_service(
            "/home/u/.local/bin/tbt-daemon",
            &[],
            "/home/u/.local/share/tokenburntrack",
        );
        assert!(unit.contains("\nExecStart=/home/u/.local/bin/tbt-daemon --once\n"));
        assert!(unit.contains("\nReadWritePaths=/home/u/.local/share/tokenburntrack\n"));
        assert!(unit.contains("\nType=oneshot\n") && unit.contains("\nNice=10\n"));
        assert!(!unit.contains("%h"));
    }

    #[test]
    fn service_quotes_and_escapes() {
        let unit = systemd_service(
            "/opt/my apps/tbt-daemon",
            &args(&["--config", "/c/100%/$x.toml"]),
            "/data/my \"dir\"",
        );
        assert!(
            unit.contains(
                "\nExecStart=\"/opt/my apps/tbt-daemon\" --once --config /c/100%%/$$x.toml\n"
            ),
            "{unit}"
        );
        assert!(
            unit.contains("\nReadWritePaths=\"/data/my \\\"dir\\\"\"\n"),
            "{unit}"
        );
    }

    #[test]
    fn plist_fills_paths_and_args() {
        let plist = launchd_plist(
            "/Users/u/bin/tbt-daemon",
            &args(&["--data-dir", "/Users/u/R&D data"]),
            "/Users/u/Library/Application Support/tokenburntrack/logs",
        );
        assert_well_formed_xml(&plist);
        assert!(plist.contains(
            "        <string>/Users/u/bin/tbt-daemon</string>\n        <string>--once</string>\n        <string>--data-dir</string>\n        <string>/Users/u/R&amp;D data</string>\n"
        ), "{plist}");
        assert!(plist.contains(
            "<string>/Users/u/Library/Application Support/tokenburntrack/logs/launchd-stderr.log</string>"
        ));
        assert!(plist.contains("<integer>3600</integer>") && !plist.contains('@'));
    }

    #[test]
    fn task_xml_fills_command_and_arguments() {
        let xml = task_xml(
            r"C:\Program Files\TBT\tbt-daemon.exe",
            &args(&["--data-dir", r"D:\tbt data\"]),
        );
        assert_well_formed_xml(&xml);
        assert!(xml.contains(r"<Command>C:\Program Files\TBT\tbt-daemon.exe</Command>"));
        assert!(
            xml.contains(r#"<Arguments>--once --data-dir &quot;D:\tbt data\\&quot;</Arguments>"#),
            "{xml}"
        );
        assert!(xml.contains("<Interval>PT1H</Interval>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
    }

    #[test]
    fn desktop_exec() {
        assert_eq!(desktop_entry(None), DESKTOP);
        let entry = desktop_entry(Some("/home/u/.local/bin/tbt-app"));
        assert!(entry.contains("\nExec=/home/u/.local/bin/tbt-app\n"));
        assert!(entry.contains("\nIcon=tbt-app\n"));
        let entry = desktop_entry(Some("/home/u/my apps/100%/tbt-app"));
        assert!(
            entry.contains("\nExec=\"/home/u/my apps/100%%/tbt-app\"\n"),
            "{entry}"
        );
        let entry = desktop_entry(Some("/a/$b"));
        assert!(entry.contains("\nExec=\"/a/\\\\$b\"\n"), "{entry}");
    }

    #[test]
    fn windows_arg_quoting() {
        assert_eq!(windows_arg("--once"), "--once");
        assert_eq!(windows_arg(r"C:\a b\c"), r#""C:\a b\c""#);
        assert_eq!(windows_arg(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(windows_arg(r#"a "b""#), r#""a \"b\"""#);
        assert_eq!(windows_arg(""), r#""""#);
    }
}
