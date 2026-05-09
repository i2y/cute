#pragma once

#include "element.hpp"

#include <QObject>
#include <functional>
#include <memory>
#include <set>

namespace cute::ui {

class BuildOwner;

/// Reactive node in the UI tree. Subclasses implement build() to return a
/// fresh Element tree; calling requestRebuild() schedules a rebuild for the
/// next frame via BuildOwner.
class Component : public QObject {
    Q_OBJECT
public:
    explicit Component(QObject* parent = nullptr);
    ~Component() override;

    virtual std::unique_ptr<Element> build() = 0;

    void requestRebuild();

    Element* currentRoot() const noexcept { return current_root_.get(); }

    void setBuildOwner(BuildOwner* owner) { build_owner_ = owner; }
    BuildOwner* buildOwner() const noexcept { return build_owner_; }

    /// Calls build() and stores the result; invoked by Window or BuildOwner.
    void rebuildSelf();

signals:
    void rebuilt();

protected:
    std::unique_ptr<Element> current_root_;
    BuildOwner* build_owner_ = nullptr;
};

/// Collects dirty Components and flushes them on demand. Window registers a
/// callback so an external rebuild request triggers a single repaint instead
/// of a continuous render loop.
class BuildOwner {
public:
    void scheduleBuild(Component* c);
    void flush();
    bool hasDirty() const noexcept { return !dirty_.empty(); }

    /// Called every time scheduleBuild is invoked. Window uses this to schedule
    /// a single QWindow::requestUpdate, so idle UIs don't burn a CPU core.
    void setOnDirty(std::function<void()> on_dirty) { on_dirty_ = std::move(on_dirty); }

private:
    std::set<Component*> dirty_;
    std::function<void()> on_dirty_;
};

} // namespace cute::ui
