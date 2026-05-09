//! Pure data types shared between the libclang-driven scraper
//! ([`crate::clang_walk`], in the bin) and the emitter
//! ([`crate::emit`], in the lib). Kept here so the lib half stays
//! free of any `clang` crate dependency — that lets the emit tests
//! run under `cargo test --lib` without needing libclang.dylib at
//! load time.

use crate::typesystem::ClassSpec;

#[derive(Debug, Clone)]
pub struct Method {
    pub name: String,
    pub params: Vec<Param>,
    pub return_ty: CuteType,
    /// True when this method was produced by lifting a Qt
    /// `bool* ok = nullptr` out-parameter into Cute's `!T` shape.
    /// The original `bool*` parameter is dropped from `params`,
    /// and `return_ty` is the lifted form (the emitter renders it
    /// with a leading `!`). The original non-lifted method is also
    /// emitted so codegen can keep using the bool*-out-arg form
    /// until support for the lifted shape lands.
    pub lifted_bool_ok: bool,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: CuteType,
}

#[derive(Debug, Clone)]
pub enum CuteType {
    /// Maps to a Cute type name (`Int`, `Bool`, `Float`, `String`,
    /// or a Qt value class like `QPoint`).
    Named(String),
    /// `void`-returning method — `.qpi` syntax omits the return type.
    Void,
}

pub struct CollectedClass {
    pub spec: ClassSpec,
    pub methods: Vec<Method>,
    /// Methods that fall inside a `signals:` / `Q_SIGNALS:` access
    /// section of the C++ class. Empty for `kind = "value"` classes
    /// (signals are a Q_OBJECT-only thing). Detected by tokenising
    /// the class body and tracking the active access section.
    pub signals: Vec<Method>,
    /// Q_PROPERTYs scraped from the C++ macro source. Empty for
    /// `kind = "value"` (no properties on plain value types) and
    /// for object classes whose header has no Q_PROPERTY lines.
    pub properties: Vec<Property>,
    /// Detected C++ base class name, only set for `kind = "object"`.
    /// `super_name` from the typesystem overrides this when set.
    pub detected_super: Option<String>,
    /// `kind = "enum"` only — variant (name, value-text) pairs
    /// scraped from libclang's EnumDecl. Value text is the source-
    /// verbatim splice (literal int, hex, expression) so codegen
    /// can emit it as-is into the Cute decl.
    pub enum_variants: Vec<EnumVariantInfo>,
}

#[derive(Debug, Clone)]
pub struct EnumVariantInfo {
    pub name: String,
    /// Source text of the explicit value when the enumerator has
    /// one (`AlignLeft = 0x0001`). None for variants that take
    /// the C++ default-progression value.
    pub value_text: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Property {
    pub name: String,
    pub ty: CuteType,
}
