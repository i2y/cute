//! Emit `.qpi` text from collected classes.
//!
//! Output shape mirrors the handcrafted stdlib bindings: optional
//! leading file-level header comment, then one `extern value <Name>
//! { ... }` block per class, separated by a blank line. Per-class
//! comments (when supplied) are emitted right above the block.
//!
//! Param-name overrides are applied here, so the clang walker can
//! stay pure traversal.

use crate::types::{CollectedClass, CuteType, Method};
use crate::typesystem::ClassKind;

pub fn emit(file_header: Option<&str>, classes: &[CollectedClass]) -> String {
    let mut out = String::new();
    if let Some(h) = file_header {
        for line in h.lines() {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    for (i, c) in classes.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(comment) = &c.spec.comment {
            for line in comment.lines() {
                out.push_str("# ");
                out.push_str(line);
                out.push('\n');
            }
        }
        match c.spec.kind {
            ClassKind::Value => {
                emit_value_class(&mut out, &c.spec.name, &c.methods, &c.spec.params)
            }
            ClassKind::Object => emit_object_class(&mut out, c),
            ClassKind::Enum => emit_enum_decl(&mut out, c),
            ClassKind::Flags => emit_flags_decl(&mut out, c),
        }
    }
    out
}

fn emit_enum_decl(out: &mut String, c: &crate::types::CollectedClass) {
    out.push_str("extern enum ");
    if let Some(ns) = &c.spec.cpp_namespace {
        // `Qt::AlignmentFlag` form — the parser splits on `::` and
        // stores the prefix as the C++ namespace, the last segment
        // as the Cute-side type name.
        out.push_str(&format!("{ns}::"));
    }
    out.push_str(&format!("{} {{\n", c.spec.name));
    for v in &c.enum_variants {
        // Skip variants whose explicit value uses bitwise / shift
        // operators Cute doesn't currently parse (`|`, `&`, `^`,
        // `<<`, `>>`). The variant itself isn't useful in Cute
        // without the value (e.g. `AlignHorizontal_Mask = ... | ...
        // | ...` — the mask only matters via the OR), so we drop
        // it entirely with a `# skipped` marker so the gap is
        // visible. Affected variants are typically derived "Mask" /
        // "All" entries, not the primary named flags.
        if let Some(text) = &v.value_text {
            if text.contains('|')
                || text.contains('&')
                || text.contains('^')
                || text.contains('<')
                || text.contains('>')
            {
                out.push_str(&format!(
                    "  # skipped {}: value uses bitwise op (`{}`) — Cute lacks these operators\n",
                    v.name, text
                ));
                continue;
            }
        }
        out.push_str(&format!("  {}", v.name));
        if let Some(text) = &v.value_text {
            out.push_str(&format!(" = {text}"));
        }
        out.push('\n');
    }
    out.push_str("}\n");
}

fn emit_flags_decl(out: &mut String, c: &crate::types::CollectedClass) {
    let of = c.spec.flags_of.as_deref().unwrap_or("UNKNOWN");
    out.push_str("extern flags ");
    if let Some(ns) = &c.spec.cpp_namespace {
        out.push_str(&format!("{ns}::"));
    }
    out.push_str(&format!("{} of {}\n", c.spec.name, of));
}

fn emit_value_class(
    out: &mut String,
    name: &str,
    methods: &[Method],
    params_overrides: &std::collections::BTreeMap<String, Vec<String>>,
) {
    out.push_str(&format!("extern value {name} {{\n"));
    emit_methods(out, methods, params_overrides);
    out.push_str("}\n");
}

fn emit_object_class(out: &mut String, c: &CollectedClass) {
    // Three states for the super clause:
    //   - typesystem `super_name = "X"`            → `class C < X {`
    //   - libclang sees a `BaseSpecifier`          → `class C < <detected> {`
    //   - neither (C++ class has no base)          → `class C {` (no super)
    // The last form covers Qt classes that aren't QObject-derived
    // (QNetworkRequest, QFileInfo, ...). Defaulting to QObject in
    // that case would make Cute think they have a QObject API
    // surface they actually don't.
    let super_name = c
        .spec
        .super_name
        .clone()
        .or_else(|| c.detected_super.clone());
    match super_name {
        Some(name) => out.push_str(&format!("class {} < {} {{\n", c.spec.name, name)),
        None => out.push_str(&format!("class {} {{\n", c.spec.name)),
    }
    for p in &c.properties {
        out.push_str(&format!("  prop {} : {}\n", p.name, render_type(&p.ty)));
    }
    if !c.properties.is_empty() && !c.signals.is_empty() {
        out.push('\n');
    }
    for s in &c.signals {
        out.push_str("  signal ");
        out.push_str(&s.name);
        if !s.params.is_empty() {
            // Same two-tier (sigtag + plain) override lookup the
            // method emitter uses, so signals like
            // `stateChanged(int)` can be renamed in the typesystem
            // without having to switch their emission shape.
            let sig_key = signature_key(&s.name, &s.params);
            let overrides = c
                .spec
                .params
                .get(&sig_key)
                .or_else(|| c.spec.params.get(&s.name));
            out.push('(');
            let parts: Vec<String> = s
                .params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let pname = overrides
                        .and_then(|v| v.get(i))
                        .cloned()
                        .unwrap_or_else(|| p.name.clone());
                    format!("{pname}: {}", render_type(&p.ty))
                })
                .collect();
            out.push_str(&parts.join(", "));
            out.push(')');
        }
        out.push('\n');
    }
    if !c.methods.is_empty() && (!c.properties.is_empty() || !c.signals.is_empty()) {
        out.push('\n');
    }
    emit_methods(out, &c.methods, &c.spec.params);
    out.push_str("}\n");
}

fn emit_methods(
    out: &mut String,
    methods: &[Method],
    params_overrides: &std::collections::BTreeMap<String, Vec<String>>,
) {
    for m in methods {
        out.push_str("  fn ");
        out.push_str(&m.name);
        if !m.params.is_empty() {
            // Two-tier lookup: signature-keyed override
            // (`params."moveTo(QPoint)" = ["p"]`) takes precedence
            // over a plain method-name override applied positionally
            // to every overload. The signature key uses the
            // Cute-side type names so a typesystem author can author
            // it from the auto-gen output without having to think
            // about the underlying C++ types.
            let sig_key = signature_key(&m.name, &m.params);
            let overrides = params_overrides
                .get(&sig_key)
                .or_else(|| params_overrides.get(&m.name));
            out.push('(');
            let parts: Vec<String> = m
                .params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let pname = overrides
                        .and_then(|v| v.get(i))
                        .cloned()
                        .unwrap_or_else(|| p.name.clone());
                    format!("{pname}: {}", render_type(&p.ty))
                })
                .collect();
            out.push_str(&parts.join(", "));
            out.push(')');
        }
        // Lifted variant: emit `!T` return + `@lifted_bool_ok` marker.
        // Codegen reads the marker at call sites and synthesizes the
        // `bool *_ok` IIFE wrapper so the Cute API is just `Result<T>`.
        // Raw entry was suppressed in clang_walk (lift_only branch).
        if m.lifted_bool_ok {
            match &m.return_ty {
                CuteType::Void => {}
                other => {
                    out.push_str(" !");
                    out.push_str(&render_type(other));
                }
            }
            out.push_str(" @lifted_bool_ok\n");
            continue;
        }
        match &m.return_ty {
            CuteType::Void => {} // .qpi convention: omit return type for void
            other => {
                out.push(' ');
                out.push_str(&render_type(other));
            }
        }
        out.push('\n');
    }
}

fn signature_key(name: &str, params: &[crate::types::Param]) -> String {
    let types: Vec<String> = params.iter().map(|p| render_type(&p.ty)).collect();
    format!("{name}({})", types.join(","))
}

fn render_type(t: &CuteType) -> String {
    match t {
        CuteType::Named(s) => s.clone(),
        CuteType::Void => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Method, Param};

    fn lifted_method(name: &str, params: Vec<Param>, ret: CuteType) -> Method {
        Method {
            name: name.to_string(),
            params,
            return_ty: ret,
            lifted_bool_ok: true,
        }
    }

    fn p(n: &str, ty: &str) -> Param {
        Param {
            name: n.to_string(),
            ty: CuteType::Named(ty.to_string()),
        }
    }

    #[test]
    fn lifted_bool_ok_emits_attribute_marker_with_bang_t_return() {
        // Lifted variants (bool* ok stripped, return wrapped in `!`)
        // are emitted as `fn name(args) !T @lifted_bool_ok`. Codegen
        // reads the marker and synthesizes the bool*-out-arg IIFE
        // wrapper so the Cute API is a clean `Result<T>`. The raw
        // form is suppressed by clang_walk in `lift_only` mode, but
        // the emitter still handles raw entries the same way for
        // methods that don't have a bool*-ok pattern.
        let methods = vec![
            lifted_method("toInt", vec![], CuteType::Named("Int".into())),
            lifted_method(
                "toInt",
                vec![p("base", "Int")],
                CuteType::Named("Int".into()),
            ),
        ];
        let mut out = String::new();
        emit_methods(&mut out, &methods, &Default::default());
        assert!(
            out.contains("  fn toInt !Int @lifted_bool_ok\n"),
            "missing nullary lifted:\n{out}"
        );
        assert!(
            out.contains("  fn toInt(base: Int) !Int @lifted_bool_ok\n"),
            "missing arity-1 lifted:\n{out}"
        );
        // No raw `fn toInt Int` should be present in this snippet
        // since the input only had lifted entries.
        assert!(
            !out.contains("  fn toInt Int\n"),
            "lifted-only emit should not produce a raw entry:\n{out}"
        );
    }
}
