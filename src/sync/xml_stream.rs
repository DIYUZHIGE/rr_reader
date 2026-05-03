use anyhow::Result;

const XML_STREAM_BUFFER_LIMIT: usize = 16 * 1024;
const TAG_CONTENTS: &str = "Contents";
const TAG_KEY: &str = "Key";
const TAG_SIZE: &str = "Size";
const TAG_ETAG: &str = "ETag";
const TAG_NEXT_TOKEN: &str = "NextContinuationToken";

#[derive(Clone, Debug)]
pub(super) struct RemoteEntry {
    pub key: String,
    pub size: u64,
    pub etag: String,
}

pub(super) struct ListXmlStreamParser {
    buf: Vec<u8>,
    entries: Vec<RemoteEntry>,
    next_token: Option<String>,
}

impl ListXmlStreamParser {
    pub(super) fn new() -> Self {
        Self {
            buf: Vec::with_capacity(2048),
            entries: Vec::new(),
            next_token: None,
        }
    }

    pub(super) fn push(&mut self, incoming: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(incoming);
        self.consume_complete_blocks();
        self.capture_next_token_if_present();
        self.trim_prefix();

        if self.buf.len() > XML_STREAM_BUFFER_LIMIT {
            return Err(anyhow::anyhow!(
                "list XML parser buffer exceeded {} bytes",
                XML_STREAM_BUFFER_LIMIT
            ));
        }

        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<(Vec<RemoteEntry>, Option<String>)> {
        self.consume_complete_blocks();
        self.capture_next_token_if_present();
        Ok((self.entries, self.next_token))
    }

    fn consume_complete_blocks(&mut self) {
        loop {
            let Some((_, after_start)) = find_open_tag(&self.buf, TAG_CONTENTS) else {
                break;
            };
            let Some((end, close_end)) = find_close_tag(&self.buf[after_start..], TAG_CONTENTS)
                .map(|(rel_start, rel_end)| (after_start + rel_start, after_start + rel_end))
            else {
                break;
            };

            let block = &self.buf[after_start..end];
            let key = extract_xml_text_bytes(block, TAG_KEY)
                .map(|s| xml_unescape(&s))
                .unwrap_or_default();
            let size = extract_xml_text_bytes(block, TAG_SIZE)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let etag = extract_xml_text_bytes(block, TAG_ETAG)
                .map(|s| xml_unescape(&s).trim_matches('"').to_string())
                .unwrap_or_default();

            if !key.is_empty() {
                self.entries.push(RemoteEntry { key, size, etag });
            }

            self.buf.drain(..close_end);
        }
    }

    fn capture_next_token_if_present(&mut self) {
        if self.next_token.is_some() {
            return;
        }

        let Some((_, after_start)) = find_open_tag(&self.buf, TAG_NEXT_TOKEN) else {
            return;
        };
        let Some((end, _)) = find_close_tag(&self.buf[after_start..], TAG_NEXT_TOKEN)
            .map(|(rel_start, rel_end)| (after_start + rel_start, after_start + rel_end))
        else {
            return;
        };
        let token_raw = String::from_utf8_lossy(&self.buf[after_start..end]).to_string();
        let token = xml_unescape(&token_raw);
        if !token.is_empty() {
            self.next_token = Some(token);
        }
    }

    fn trim_prefix(&mut self) {
        if self.buf.len() <= XML_STREAM_BUFFER_LIMIT / 2 {
            return;
        }

        if let Some((pos, _)) = find_open_tag(&self.buf, TAG_CONTENTS) {
            if pos > 0 {
                self.buf.drain(..pos);
            }
            return;
        }

        if let Some((pos, _)) = find_open_tag(&self.buf, TAG_NEXT_TOKEN) {
            if pos > 0 {
                self.buf.drain(..pos);
            }
            return;
        }

        let keep = XML_STREAM_BUFFER_LIMIT / 4;
        if self.buf.len() > keep {
            let drop_len = self.buf.len() - keep;
            self.buf.drain(..drop_len);
        }
    }
}

fn extract_xml_text_bytes(haystack: &[u8], tag: &str) -> Option<String> {
    let (_, start) = find_open_tag(haystack, tag)?;
    let (end, _) = find_close_tag(&haystack[start..], tag)?;
    Some(String::from_utf8_lossy(&haystack[start..start + end]).to_string())
}

fn find_open_tag(haystack: &[u8], local_name: &str) -> Option<(usize, usize)> {
    find_xml_tag(haystack, local_name, false)
}

fn find_close_tag(haystack: &[u8], local_name: &str) -> Option<(usize, usize)> {
    find_xml_tag(haystack, local_name, true)
}

fn find_xml_tag(haystack: &[u8], local_name: &str, closing: bool) -> Option<(usize, usize)> {
    let mut i = 0usize;
    while i < haystack.len() {
        if haystack[i] != b'<' {
            i += 1;
            continue;
        }
        let name_start = if closing { i + 2 } else { i + 1 };
        if closing && haystack.get(i + 1) != Some(&b'/') {
            i += 1;
            continue;
        }
        if name_start >= haystack.len()
            || haystack[name_start] == b'!'
            || haystack[name_start] == b'?'
        {
            i += 1;
            continue;
        }
        let mut name_end = name_start;
        while name_end < haystack.len()
            && !matches!(
                haystack[name_end],
                b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n'
            )
        {
            name_end += 1;
        }
        let full_name = core::str::from_utf8(&haystack[name_start..name_end]).ok()?;
        let matched = full_name == local_name
            || full_name
                .rsplit_once(':')
                .map(|(_, suffix)| suffix == local_name)
                .unwrap_or(false);
        if !matched {
            i += 1;
            continue;
        }
        let mut tag_end = name_end;
        while tag_end < haystack.len() && haystack[tag_end] != b'>' {
            tag_end += 1;
        }
        if tag_end >= haystack.len() {
            return None;
        }
        return Some((i, tag_end + 1));
    }
    None
}

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                if let Ok(value) = u32::from_str_radix(&entity[2..], 16) {
                    if let Some(ch) = char::from_u32(value) {
                        out.push(ch);
                    }
                }
            }
            _ if entity.starts_with('#') => {
                if let Ok(value) = entity[1..].parse::<u32>() {
                    if let Some(ch) = char::from_u32(value) {
                        out.push(ch);
                    }
                }
            }
            _ => {
                out.push('&');
                out.push_str(entity);
                out.push(';');
            }
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}
