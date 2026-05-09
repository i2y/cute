# Standalone QWindow + QRhi + QCanvasPainterFactory smoke test.
add_executable(hello_window hello_window.cpp)
set_target_properties(hello_window PROPERTIES AUTOMOC ON)
target_link_libraries(hello_window PRIVATE cute_ui::cute_ui)

# Bare QGuiApplication + QWindow (no QRhi, no Canvas Painter) for isolating
# Qt-only behavior from cute_ui.
add_executable(hello_minimal hello_minimal.cpp)
target_link_libraries(hello_minimal PRIVATE Qt6::Core Qt6::Gui)

# Hand-written equivalent of cute-build output for a Counter widget.
add_executable(counter_handcoded counter_handcoded.cpp)
set_target_properties(counter_handcoded PROPERTIES AUTOMOC ON)
target_link_libraries(counter_handcoded PRIVATE cute_ui::cute_ui)

# Hand-written TextField smoke test — exercises focus, keyboard, IME.
add_executable(textfield_handcoded textfield_handcoded.cpp)
set_target_properties(textfield_handcoded PROPERTIES AUTOMOC ON)
target_link_libraries(textfield_handcoded PRIVATE cute_ui::cute_ui)
