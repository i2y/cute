// Cute runtime: QMetaObject helpers (currently empty).
//
// Targeting Qt 6.9+, the moc-replacement code lives in two places:
// - The generated header uses Qt's `Q_OBJECT` macro to declare the
//   `qt_create_metaobjectdata<Tag>` member template, the
//   `qt_staticMetaObject{Static,Relocating}Content` variable templates,
//   `staticMetaObject`, and the `metaObject()/qt_metacast/qt_metacall`
//   virtual overrides. (Cute users do not write Q_OBJECT in `.cute` source;
//   it appears only in cutec's generated `.h`.)
// - The generated `.cpp` specializes `qt_create_metaobjectdata<Tag>()`
//   using `QtMocHelpers::{StringRefStorage, UintData, SignalData,
//   MethodData, PropertyData}` from `<QtCore/qtmochelpers.h>`. cute-meta
//   emits this specialization plus the four virtual function bodies and
//   each signal's `QMetaObject::activate` body.
//
// As a result, this runtime header has no remaining surface. It is kept
// for symmetry with `cute_arc.h` / `cute_error.h` / `cute_string.h` and
// as a future home for any moc-replacement helpers that do not naturally
// live inside Qt's own headers.

#pragma once
