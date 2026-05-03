use anyhow::Result;

const XML_CONTENTS_OPEN: &[u8] = b"<Contents>";
const XML_CONTENTS_CLOSE: &[u8] = b"</Contents>";
const XML_KEY_OPEN: &[u8] = b"<Key>";
const XML_KEY_CLOSE: &[u8] = b"</Key>";
const XML_SIZE_OPEN: &[u8] = b"<Size>";
const XML_SIZE_CLOSE: &[u8] = b"</Size>";
const XML_ETAG_OPEN: &[u8] = b"<ETag>";
const XML_ETAG_CLOSE: &[u8] = b"</ETag>";
const XML_NEXT_TOKEN_OPEN: &[u8] = b"<NextContinuationToken>";
const XML_NEXT_TOKEN_CLOSE: &[u8] = b"</NextContinuationToken>";
const XML_STREAM_BUFFER_LIMIT: usize = 8 * 1024;

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
            let Some(start) = find_subslice(&self.buf, XML_CONTENTS_OPEN) else {
                break;
            };
            let after_start = start + XML_CONTENTS_OPEN.len();
            let Some(rel_end) = find_subslice(&self.buf[after_start..], XML_CONTENTS_CLOSE) else {
                break;
            };
            let end = after_start + rel_end;

            let block = &self.buf[after_start..end];
            let key = extract_xml_text_bytes(block, XML_KEY_OPEN, XML_KEY_CLOSE)
                .map(|s| xml_unescape(&s))
                .unwrap_or_default();
            let size = extract_xml_text_bytes(block, XML_SIZE_OPEN, XML_SIZE_CLOSE)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let etag = extract_xml_text_bytes(block, XML_ETAG_OPEN, XML_ETAG_CLOSE)
                .map(|s| xml_unescape(&s).trim_matches('"').to_string())
                .unwrap_or_default();

            if !key.is_empty() {
                self.entries.push(RemoteEntry { key, size, etag });
            }

            let consume_end = end + XML_CONTENTS_CLOSE.len();
            self.buf.drain(..consume_end);
        }
    }

    fn capture_next_token_if_present(&mut self) {
        if self.next_token.is_some() {
            return;
        }

        let Some(start) = find_subslice(&self.buf, XML_NEXT_TOKEN_OPEN) else {
            return;
        };
        let after_start = start + XML_NEXT_TOKEN_OPEN.len();
        let Some(rel_end) = find_subslice(&self.buf[after_start..], XML_NEXT_TOKEN_CLOSE) else {
            return;
        };
        let end = after_start + rel_end;
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

        if let Some(pos) = find_subslice(&self.buf, XML_CONTENTS_OPEN) {
            if pos > 0 {
                self.buf.drain(..pos);
            }
            return;
        }

        if let Some(pos) = find_subslice(&self.buf, XML_NEXT_TOKEN_OPEN) {
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

fn extract_xml_text_bytes(haystack: &[u8], open: &[u8], close: &[u8]) -> Option<String> {
    let start = find_subslice(haystack, open)? + open.len();
    let rel_end = find_subslice(&haystack[start..], close)?;
    let end = start + rel_end;
    Some(String::from_utf8_lossy(&haystack[start..end]).to_string())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}
