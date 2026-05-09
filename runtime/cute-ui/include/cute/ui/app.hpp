#pragma once

#include "paint.hpp"

#include <QGuiApplication>

namespace cute::ui {

class Component;

/// Generated `int main` boots cute::ui via:
///   cute::ui::App app(argc, argv);
///   MyView view;
///   return app.run(&view);
class App : public QGuiApplication {
    Q_OBJECT
public:
    App(int& argc, char** argv);
    ~App() override;

    /// Creates a Window over `root`, shows it, and runs the event loop.
    /// Caller retains ownership of `root`. `theme` selects the initial
    /// Style; live switching via Window::setTheme remains available
    /// (Cmd/Ctrl+T toggles by default).
    int run(Component* root, Theme theme = Theme::Dark);
};

} // namespace cute::ui
