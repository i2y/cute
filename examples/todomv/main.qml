// examples/todomv/main.qml
//
// Material-themed Todo list driven entirely by the Cute-side TodoApp /
// TodoItem QObjects. Every interaction (toggle, add, remove) routes
// through Q_INVOKABLE methods that cutec emitted from `app.cute` -
// no JS-side data model, no hand-written C++.

import QtQuick
import QtQuick.Controls
import QtQuick.Controls.Material
import QtQuick.Layouts
import Cute.TodoMV 1.0

ApplicationWindow {
    id: root
    visible: true
    width: 480
    height: 640
    title: "Cute TodoMV"

    Material.theme: Material.Light
    Material.primary: Material.Indigo
    Material.accent: Material.Indigo

    TodoApp {
        id: app
    }

    Component.onCompleted: {
        app.add_todo("Try Cute")
        app.add_todo("Build a Plasma widget in 30 lines")
        app.add_todo("Ship a Cute compiler with no moc")
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 20
        spacing: 16

        Label {
            text: "Cute TodoMV"
            font.pixelSize: 28
            font.weight: Font.Medium
            color: Material.foreground
            Layout.alignment: Qt.AlignHCenter
        }

        Label {
            text: "powered by Cute → C++ → Qt 6 (no moc, no main.cpp)"
            font.pixelSize: 12
            opacity: 0.6
            Layout.alignment: Qt.AlignHCenter
        }

        // Add form
        RowLayout {
            Layout.fillWidth: true
            spacing: 8

            TextField {
                id: input
                Layout.fillWidth: true
                placeholderText: "What needs doing?"
                font.pixelSize: 14
                onAccepted: addBtn.activate()
            }
            Button {
                id: addBtn
                text: "Add"
                highlighted: true
                enabled: input.text.length > 0
                function activate() {
                    if (input.text.length > 0) {
                        app.add_todo(input.text)
                        input.clear()
                        input.forceActiveFocus()
                    }
                }
                onClicked: activate()
            }
        }

        ListView {
            id: list
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            model: app.items
            spacing: 4

            delegate: ItemDelegate {
                width: list.width

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 12
                    anchors.rightMargin: 12
                    spacing: 8

                    CheckBox {
                        checked: modelData.done
                        onClicked: app.toggle_at(index)
                    }
                    Label {
                        Layout.fillWidth: true
                        text: modelData.text
                        font.pixelSize: 16
                        color: modelData.done ? Material.color(Material.Grey) : Material.foreground
                        font.strikeout: modelData.done
                    }
                    Button {
                        text: "✕"
                        flat: true
                        implicitWidth: 36
                        Material.foreground: Material.color(Material.Grey)
                        onClicked: app.remove_at(index)
                    }
                }
            }
        }

        Label {
            Layout.alignment: Qt.AlignHCenter
            text: app.count + (app.count === 1 ? " item" : " items")
            opacity: 0.6
            font.pixelSize: 12
        }
    }
}
