// e2e: instantiate the cutec-generated TodoItem, connect to its signal,
// flip the property, and verify the signal fired. No moc invocation.
//
// Build: see CMakeLists.txt in this directory. The generated TodoItem.h/.cpp
// is produced by `cutec build ../../examples/todomv/todo_item.cute --out-dir generated`
// (handled by the CMake target).

#include "todo_item.h"

#include <QCoreApplication>
#include <QObject>
#include <QVariant>
#include <iostream>

int main(int argc, char** argv) {
    QCoreApplication app(argc, argv);

    TodoItem item;
    item.setText(QStringLiteral("milk"));

    int hits = 0;
    QObject::connect(&item, &TodoItem::state_changed, [&]() { ++hits; });

    if (item.done()) {
        std::cerr << "expected done=false initially\n";
        return 1;
    }
    if (item.text() != QStringLiteral("milk")) {
        std::cerr << "text round-trip failed\n";
        return 1;
    }

    item.toggle();
    if (!item.done()) {
        std::cerr << "expected done=true after toggle\n";
        return 1;
    }
    if (hits != 1) {
        std::cerr << "expected 1 signal hit, got " << hits << "\n";
        return 1;
    }

    // QMetaObject reflection: use it to call toggle() by name.
    bool ok = QMetaObject::invokeMethod(&item, "toggle", Qt::DirectConnection);
    if (!ok) {
        std::cerr << "QMetaObject::invokeMethod(\"toggle\") failed\n";
        return 1;
    }
    if (item.done()) {
        std::cerr << "expected done=false after second toggle\n";
        return 1;
    }
    if (hits != 2) {
        std::cerr << "expected 2 signal hits, got " << hits << "\n";
        return 1;
    }

    // Property reflection: read+write `text` through QMetaObject.
    item.setProperty("text", QStringLiteral("eggs"));
    if (item.text() != QStringLiteral("eggs")) {
        std::cerr << "QMetaObject property write failed\n";
        return 1;
    }
    if (item.property("text").toString() != QStringLiteral("eggs")) {
        std::cerr << "QMetaObject property read failed\n";
        return 1;
    }

    std::cout << "ok: TodoItem signals + properties + invokable all work via QMetaObject "
              << "(" << hits << " signal hits)\n";
    return 0;
}
