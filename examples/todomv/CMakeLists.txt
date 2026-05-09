cmake_minimum_required(VERSION 3.21)
project(cute_todomv CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
# AUTOMOC stays OFF: cutec emits the qt_create_metaobjectdata<Tag> template
# specialization itself (Qt 6.9+ moc-output form). AUTORCC packages main.qml.
set(CMAKE_AUTOMOC OFF)
set(CMAKE_AUTORCC ON)

list(APPEND CMAKE_PREFIX_PATH "/opt/homebrew/opt/qt")
find_package(Qt6 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2)

if(NOT DEFINED CUTEC OR CUTEC STREQUAL "")
    set(CUTEC "${CMAKE_SOURCE_DIR}/../../target/debug/cute")
endif()

set(GEN_DIR "${CMAKE_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${GEN_DIR}")

# Single Cute file holds the QObject class AND `fn main { qml_app(...) }`.
# cutec's qml_app intrinsic emits the QGuiApplication + qmlRegisterType +
# QQmlApplicationEngine boot sequence directly into app.cpp, so this
# project has zero hand-written C++.
add_custom_command(
    OUTPUT "${GEN_DIR}/app.h" "${GEN_DIR}/app.cpp"
    COMMAND ${CUTEC} build "${CMAKE_SOURCE_DIR}/app.cute" --out-dir "${GEN_DIR}"
    DEPENDS "${CMAKE_SOURCE_DIR}/app.cute" ${CUTEC}
    COMMENT "Compiling Cute -> C++ (TodoMV, including main)"
    VERBATIM
)

qt_add_executable(cute_todomv
    qml.qrc
    "${GEN_DIR}/app.cpp"
)

target_include_directories(cute_todomv PRIVATE
    "${GEN_DIR}"
    "${CMAKE_SOURCE_DIR}/../../runtime/cpp"
)

target_link_libraries(cute_todomv PRIVATE
    Qt6::Core
    Qt6::Gui
    Qt6::Qml
    Qt6::Quick
    Qt6::QuickControls2
)
