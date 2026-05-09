cmake_minimum_required(VERSION 3.21)
project(cute_todomv_cli CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC OFF)

list(APPEND CMAKE_PREFIX_PATH "/opt/homebrew/opt/qt")
find_package(Qt6 REQUIRED COMPONENTS Core)

if(NOT DEFINED CUTEC OR CUTEC STREQUAL "")
    set(CUTEC "${CMAKE_SOURCE_DIR}/../../target/debug/cute")
endif()

set(GEN_DIR "${CMAKE_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${GEN_DIR}")

# Single Cute file holds the QObject class + `fn main { cli_app { ... } }`.
# cutec's cli_app intrinsic emits a QCoreApplication boot wrapper around
# the user block and links only Qt6::Core - no QML, no GUI deps.
add_custom_command(
    OUTPUT "${GEN_DIR}/app.h" "${GEN_DIR}/app.cpp"
    COMMAND ${CUTEC} build "${CMAKE_SOURCE_DIR}/app.cute" --out-dir "${GEN_DIR}"
    DEPENDS "${CMAKE_SOURCE_DIR}/app.cute" ${CUTEC}
    COMMENT "Compiling Cute -> C++ (TodoMV CLI)"
    VERBATIM
)

add_executable(cute_todomv_cli
    "${GEN_DIR}/app.cpp"
)

target_include_directories(cute_todomv_cli PRIVATE
    "${GEN_DIR}"
    "${CMAKE_SOURCE_DIR}/../../runtime/cpp"
)

target_link_libraries(cute_todomv_cli PRIVATE Qt6::Core)
