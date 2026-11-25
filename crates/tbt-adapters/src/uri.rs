pub(crate) fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = rest.get(rest.find('/')?..)?;
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(b) = path
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let decoded = String::from_utf8(out).ok()?;
    let windows_drive = decoded.len() >= 3
        && decoded.as_bytes()[2] == b':'
        && decoded.as_bytes()[1].is_ascii_alphabetic();
    Some(if windows_drive {
        decoded[1..].to_owned()
    } else {
        decoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uri_to_path("file:///home/u/my%20proj").as_deref(),
            Some("/home/u/my proj")
        );
        assert_eq!(
            file_uri_to_path("file:///c%3A/Users/u/proj").as_deref(),
            Some("c:/Users/u/proj")
        );
        assert_eq!(file_uri_to_path("vscode-remote://ssh/x"), None);
    }
}
