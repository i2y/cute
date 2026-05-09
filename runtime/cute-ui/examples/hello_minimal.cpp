// Bare QGuiApplication + QWindow, with no QRhi or Canvas Painter — used to
// isolate Qt-side issues from cute_ui rendering.

#include <QGuiApplication>
#include <QWindow>
#include <QDebug>

int main(int argc, char **argv)
{
    QGuiApplication app(argc, argv);
    qInfo() << "minimal start";

    QWindow window;
    window.setTitle(QStringLiteral("cute::ui hello_minimal"));
    window.resize(640, 480);
    window.show();
    qInfo() << "window shown";

    return app.exec();
}
