use std::collections::HashMap;

/// Render a `{{key}}` template in a single pass.
///
/// Unlike a naive repeated `String::replace`, this walks the template once, so
/// a value that itself contains `{{another_key}}` is never re-expanded and the
/// result is independent of iteration order. Unknown placeholders are left
/// verbatim.
pub fn render_template(template: &str, vars: &HashMap<&str, String>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("{{") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];

        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                match vars.get(key) {
                    Some(value) => result.push_str(value),
                    None => {
                        // Unknown placeholder: keep it untouched.
                        result.push_str("{{");
                        result.push_str(&after[..end]);
                        result.push_str("}}");
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                // No closing braces; emit the rest as-is and stop.
                result.push_str("{{");
                result.push_str(after);
                return result;
            }
        }
    }

    result.push_str(rest);
    result
}
