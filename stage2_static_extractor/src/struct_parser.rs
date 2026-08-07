use ra_ap_hir::Semantics;
use ra_ap_syntax::ast::HasName;
use ra_ap_syntax::{AstNode, ast};

use crate::types::StructInfo;
use crate::visibility::is_pub;

/// Extracts a struct's name and public fields (both named and tuple fields),
/// returning a `StructInfo`.
///
/// Methods are not resolved here; the returned `StructInfo` starts with an
/// empty methods vector.
///
/// # Parameters
///
/// * `_sema` — rust-analyzer `Semantics` (reserved for future use).
/// * `s` — The `ast::Struct` node.
///
/// # Returns
///
/// `Some(StructInfo)` if the struct has a name, `None` otherwise.
pub fn extract_struct_definition_with_public_fields(
    _sema: &Semantics<ra_ap_ide::RootDatabase>,
    s: &ast::Struct,
) -> Option<StructInfo> {
    let mut public_struct_info = StructInfo {
        name: s.name()?.to_string(),
        fields: "".to_string(),
        methods: Vec::new(),
    };
    let mut public_fields = Vec::new();

    match s.field_list() {
        Some(ast::FieldList::RecordFieldList(field_list)) => {
            for field in field_list.fields() {
                if is_pub(&field) {
                    public_fields.push(field.syntax().text().to_string());
                }
            }
        }
        Some(ast::FieldList::TupleFieldList(field_list)) => {
            for field in field_list.fields() {
                if is_pub(&field) {
                    public_fields.push(field.syntax().text().to_string());
                }
            }
        }
        None => {}
    }
    public_struct_info.fields = public_fields.join(", ");
    Some(public_struct_info)
}
