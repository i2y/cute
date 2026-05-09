#include "cute/ui/component.hpp"

#include <algorithm>
#include <vector>

namespace cute::ui {

Component::Component(QObject* parent) : QObject(parent) {}

Component::~Component() = default;

void Component::requestRebuild()
{
    if (build_owner_) {
        build_owner_->scheduleBuild(this);
    }
}

void Component::rebuildSelf()
{
    auto next = build();
    if (current_root_ && next) {
        transferStateRecursive(current_root_.get(), next.get());
    }
    current_root_ = std::move(next);
    emit rebuilt();
}

void BuildOwner::scheduleBuild(Component* c)
{
    if (!c) return;
    if (dirty_.insert(c).second && on_dirty_) {
        on_dirty_();
    }
}

void BuildOwner::flush()
{
    auto pending = std::move(dirty_);
    dirty_.clear();
    for (Component* c : pending) {
        c->rebuildSelf();
    }
}

} // namespace cute::ui
