// cute_model.h — runtime support for `prop xs : ModelList<T>` props.
//
// `cute::ModelList<T>` is the type carried by the property. It IS a
// `QAbstractItemModel` (publicly extends `QRangeModel`), so the same
// object is consumed by QML `ListView`, QtWidgets `QListView` /
// `QTableView`, etc. Mutations go through ordinary public methods —
// `xs->append(b)`, `xs->removeAt(i)`, `xs->clear()`, `xs->replace(...)` —
// each of which fires the appropriate `beginInsertRows` /
// `endInsertRows` / `beginRemoveRows` / `endRemoveRows` /
// `beginResetModel` / `endResetModel` pair so views stay in sync.
//
// No compiler magic. No "inside class only" rule. A method call that
// the type doesn't define is a regular C++ compile error, not a
// silent runtime no-op.
//
// Implementation note: ModelList stores the underlying `QList<T*>`
// itself, but the QRangeModel base needs to be constructed with a
// reference to that list — and the base runs before regular member
// initializers. We use a private "InnerHolder" base to hold the
// `QList<T*>` so it lives before the QRangeModel base is constructed,
// then pass `std::ref(this->m_inner)` to the QRangeModel ctor.
//
// **Display-role default**: QRangeModel's MultiRoleItem mode auto-
// derives one role per Q_PROPERTY of the row type (role IDs start at
// `Qt::UserRole` in metaObject order, i.e. the first user-declared
// property gets `Qt::UserRole + 0`) but does NOT map any to
// `Qt::DisplayRole`. The default QListView / QTableView delegate then
// renders blank rows. ModelList's ctor calls `setRoleNames(...)` once
// to install a `Qt::DisplayRole → "<first-property-name>"` entry so
// every read path (`data()` / `multiData()` / `itemData()`) routes
// through the same `roleProperty` lookup. Note: a `data()` override
// alone does NOT cover the QStyledItemDelegate path on Qt 6.11+,
// which goes through `multiData()` and bypasses the override
// (`qrangemodel_impl.h:1469-1477` calls `readRole(...)` directly).
// To override the displayed property in user C++, supply a custom
// QStyledItemDelegate that reads a different named role.

#pragma once

#include <QList>
#include <QModelIndex>
#include <QObject>
#include <QRangeModel>
#include <QVariant>

#include <functional>
#include <utility>

namespace cute {

namespace detail {
// Private base whose sole job is to hold the inner QList so it's
// already alive when the QRangeModel base ctor runs. C++ initialises
// bases in declaration order; ModelList lists this base first.
template <typename T>
struct ModelListInner {
    QList<T*> m_inner;
};
} // namespace detail

template <typename T>
class ModelList : private detail::ModelListInner<T>, public QRangeModel {
public:
    explicit ModelList(QObject* parent = nullptr)
        : detail::ModelListInner<T>{},
          QRangeModel(std::ref(this->m_inner), parent) {
        installDisplayRoleAlias();
    }

    explicit ModelList(QList<T*> initial, QObject* parent = nullptr)
        : detail::ModelListInner<T>{std::move(initial)},
          QRangeModel(std::ref(this->m_inner), parent) {
        installDisplayRoleAlias();
    }

    ModelList(const ModelList&) = delete;
    ModelList& operator=(const ModelList&) = delete;

    // ---- structural mutators (model-event-aware) ----

    void append(T* x) {
        const int n = this->m_inner.size();
        beginInsertRows(QModelIndex(), n, n);
        this->m_inner.append(x);
        endInsertRows();
    }

    void prepend(T* x) {
        beginInsertRows(QModelIndex(), 0, 0);
        this->m_inner.prepend(x);
        endInsertRows();
    }

    void insert(int i, T* x) {
        beginInsertRows(QModelIndex(), i, i);
        this->m_inner.insert(i, x);
        endInsertRows();
    }

    void removeAt(int i) {
        beginRemoveRows(QModelIndex(), i, i);
        this->m_inner.removeAt(i);
        endRemoveRows();
    }

    void remove(int i) { removeAt(i); }

    void removeFirst() {
        beginRemoveRows(QModelIndex(), 0, 0);
        this->m_inner.removeFirst();
        endRemoveRows();
    }

    void removeLast() {
        const int n = this->m_inner.size();
        beginRemoveRows(QModelIndex(), n - 1, n - 1);
        this->m_inner.removeLast();
        endRemoveRows();
    }

    void clear() {
        beginResetModel();
        this->m_inner.clear();
        endResetModel();
    }

    // v1: `move` rewrites as a model reset. Proper begin/endMoveRows
    // requires careful destChild adjacency arithmetic; reset is correct
    // but does a full re-fetch on the consumer side. Revisit if a real
    // demo needs single-row move animations.
    void move(int from, int to) {
        beginResetModel();
        this->m_inner.move(from, to);
        endResetModel();
    }

    // Full-replace: drop everything, install the new list, view sees
    // a model reset.
    void replace(QList<T*> newList) {
        beginResetModel();
        this->m_inner = std::move(newList);
        endResetModel();
    }

    // ---- read access (forwards to the inner QList) ----

    int size() const { return this->m_inner.size(); }
    bool isEmpty() const { return this->m_inner.isEmpty(); }
    T* at(int i) const { return this->m_inner.at(i); }
    T* operator[](int i) const { return this->m_inner[i]; }

    auto begin() const { return this->m_inner.begin(); }
    auto end() const { return this->m_inner.end(); }
    auto cbegin() const { return this->m_inner.cbegin(); }
    auto cend() const { return this->m_inner.cend(); }

    // Const access to the underlying QList for C++ interop / passing
    // to non-Cute helpers that want a plain `QList<T*>&`.
    const QList<T*>& asList() const { return this->m_inner; }

private:
    // Map `Qt::DisplayRole` to the row type's first user-declared
    // Q_PROPERTY name. Both `data()` (`qrangemodel_impl.h:1244` for
    // single-role / `:1241` for multi-role) and `multiData()`
    // (`:1474`) read through `roleProperty(role)`, which itself
    // calls `roleNames().value(role)` and looks up the result with
    // `metaObject->indexOfProperty(...)`. Adding `{DisplayRole, "title"}`
    // to roleNames means every default delegate read returns the
    // first property without us having to override either virtual.
    void installDisplayRoleAlias() {
        const QMetaObject& mo = T::staticMetaObject;
        if (mo.propertyOffset() < mo.propertyCount()) {
            auto names = roleNames();
            names[Qt::DisplayRole] = mo.property(mo.propertyOffset()).name();
            setRoleNames(names);
        }
    }
};

} // namespace cute
