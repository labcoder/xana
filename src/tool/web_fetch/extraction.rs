//! Bound rendering complexity independently of the raw HTTP byte allowance.
//! The parser is not a sandbox: input is already byte-bounded and its blocking
//! admission remains held until it exits, even after the caller times out.

const MAX_NODES: usize = 16_384;
const MAX_DEPTH: usize = 64;

pub(super) fn html(input: &[u8], width: usize) -> Result<String, html2text::Error> {
    let config = html2text::config::plain();
    let dom = config.parse_html(input)?;
    let mut pending = vec![(dom.document.clone(), 0usize)];
    let mut visited = 0usize;
    while let Some((node, depth)) = pending.pop() {
        visited += 1;
        if visited > MAX_NODES || depth > MAX_DEPTH {
            return Err(html2text::Error::Fail);
        }
        let children = node.children.borrow();
        if visited
            .saturating_add(pending.len())
            .saturating_add(children.len())
            > MAX_NODES
        {
            return Err(html2text::Error::Fail);
        }
        pending.extend(children.iter().map(|child| (child.clone(), depth + 1)));
    }
    let tree = config.dom_to_render_tree(&dom)?;
    config.render_to_string(tree, width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_links_and_text_survive_but_deep_or_wide_trees_stop_before_rendering() {
        let ordinary = html(
            b"<h1>Schedule</h1><a href='https://example.com'>7:10 PM ET</a>",
            120,
        )
        .unwrap();
        assert!(ordinary.contains("Schedule") && ordinary.contains("https://example.com"));
        let deep = format!("{}text{}", "<div>".repeat(70), "</div>".repeat(70));
        assert!(html(deep.as_bytes(), 120).is_err());
        assert!(html("<span>x</span>".repeat(MAX_NODES).as_bytes(), 120).is_err());
    }
}
