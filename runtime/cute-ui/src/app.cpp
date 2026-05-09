#include "cute/ui/app.hpp"
#include "cute/ui/component.hpp"
#include "cute/ui/window.hpp"

namespace cute::ui {

App::App(int& argc, char** argv) : QGuiApplication(argc, argv)
{
}

App::~App() = default;

int App::run(Component* root, Theme theme)
{
    Window window(root);
    window.setTheme(theme);
    window.show();
    return exec();
}

} // namespace cute::ui
