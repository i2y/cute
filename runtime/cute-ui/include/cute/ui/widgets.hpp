#pragma once

// Factory helpers used by `using namespace cute::ui::dsl;` in generated
// build() bodies, so the tree literal reads `col(text("..."), button("+"))`.

#include "element.hpp"

#include <memory>
#include <utility>

namespace cute::ui::dsl {

inline std::unique_ptr<TextElement> text(QString s) {
    return std::make_unique<TextElement>(std::move(s));
}

inline std::unique_ptr<ButtonElement> button(QString label) {
    return std::make_unique<ButtonElement>(std::move(label));
}

/// onClick-attaching overload. Lets generated code pass a lambda inline
/// while keeping the unique_ptr ownership intact for the col/row variadic.
inline std::unique_ptr<ButtonElement> button(QString label, std::function<void()> on_click) {
    auto b = std::make_unique<ButtonElement>(std::move(label));
    b->onClick(std::move(on_click));
    return b;
}

inline std::unique_ptr<RectElement> rect(QColor fill, float radius = 0.f) {
    return std::make_unique<RectElement>(fill, radius);
}

inline std::unique_ptr<TextFieldElement> textfield() {
    return std::make_unique<TextFieldElement>();
}

inline std::unique_ptr<TextFieldElement> textfield(QString placeholder) {
    auto t = std::make_unique<TextFieldElement>();
    t->setPlaceholder(std::move(placeholder));
    return t;
}

inline std::unique_ptr<ImageElement> image(QString source) {
    return std::make_unique<ImageElement>(std::move(source));
}

inline std::unique_ptr<SvgElement> svg(QString source) {
    return std::make_unique<SvgElement>(std::move(source));
}

inline std::unique_ptr<BarChartElement> barchart() {
    return std::make_unique<BarChartElement>();
}

inline std::unique_ptr<LineChartElement> linechart() {
    return std::make_unique<LineChartElement>();
}

inline std::unique_ptr<ProgressBarElement> progressbar() {
    return std::make_unique<ProgressBarElement>();
}

inline std::unique_ptr<SpinnerElement> spinner() {
    return std::make_unique<SpinnerElement>();
}

// initializer_list cannot hold move-only unique_ptr, so containers take
// variadic template args instead. Callers write: col(std::move(c1), std::move(c2))
template <class... Children>
inline std::unique_ptr<ColumnElement> col(Children&&... children) {
    auto c = std::make_unique<ColumnElement>();
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<RowElement> row(Children&&... children) {
    auto c = std::make_unique<RowElement>();
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<StackElement> stack(Children&&... children) {
    auto c = std::make_unique<StackElement>();
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<ListViewElement> listview(Children&&... children) {
    auto c = std::make_unique<ListViewElement>();
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<DataTableElement> datatable(Children&&... children) {
    auto c = std::make_unique<DataTableElement>();
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<ScrollViewElement> scrollview(Children&&... children) {
    auto c = std::make_unique<ScrollViewElement>(ScrollAxis::Vertical);
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<ScrollViewElement> hscrollview(Children&&... children) {
    auto c = std::make_unique<ScrollViewElement>(ScrollAxis::Horizontal);
    (c->addChild(std::move(children)), ...);
    return c;
}

template <class... Children>
inline std::unique_ptr<ModalElement> modal(Children&&... children) {
    auto m = std::make_unique<ModalElement>();
    (m->addChild(std::move(children)), ...);
    return m;
}

} // namespace cute::ui::dsl
