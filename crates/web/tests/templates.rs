//! Checks over the template files themselves, for mistakes that render
//! without an error.

use std::fs;

/// A page title is text. Markup inside `{% block title %}` ends up in the
/// `<title>` element, where the browser shows it as literal text in the tab.
#[test]
fn page_titles_hold_text_only() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/templates");
    let mut checked = 0;
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let src = fs::read_to_string(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut rest = src.as_str();
        while let Some(start) = rest.find("{% block title %}") {
            let after = &rest[start + "{% block title %}".len()..];
            let end = after
                .find("{% endblock %}")
                .unwrap_or_else(|| panic!("{name}: title block never ends"));
            let title = &after[..end];
            assert!(
                !title.contains('<') && !title.contains('\n'),
                "{name}: the title block holds more than a title: {title:?}"
            );
            checked += 1;
            rest = &after[end..];
        }
    }
    assert!(checked > 20, "found only {checked} title blocks");
}
