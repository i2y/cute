cmake_minimum_required(VERSION 3.21)
project(cute_qt_verify CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC OFF)  # crucial: prove cutec replaces moc

# Locate Qt 6 (homebrew default).
list(APPEND CMAKE_PREFIX_PATH "/opt/homebrew/opt/qt")
find_package(Qt6 REQUIRED COMPONENTS Core)

# Locate cutec. Prefer the workspace-built debug binary; allow override.
if(NOT DEFINED CUTEC OR CUTEC STREQUAL "")
    set(CUTEC "${CMAKE_SOURCE_DIR}/../../../target/debug/cute")
endif()

# Source: TodoItem.cute
set(CUTE_SOURCE "${CMAKE_SOURCE_DIR}/../../../examples/todomv/todo_item.cute")
set(GEN_DIR "${CMAKE_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${GEN_DIR}")

add_custom_command(
    OUTPUT "${GEN_DIR}/todo_item.h" "${GEN_DIR}/todo_item.cpp"
    COMMAND ${CUTEC} build "${CUTE_SOURCE}" --out-dir "${GEN_DIR}"
    DEPENDS "${CUTE_SOURCE}" ${CUTEC}
    COMMENT "Compiling Cute -> C++"
    VERBATIM
)

add_executable(cute_qt_verify
    main.cpp
    "${GEN_DIR}/todo_item.cpp"
)

target_include_directories(cute_qt_verify PRIVATE
    "${GEN_DIR}"
    "${CMAKE_SOURCE_DIR}/../../../runtime/cpp"
)

target_link_libraries(cute_qt_verify PRIVATE Qt6::Core)

# Marker so we can find the binary easily.
set_target_properties(cute_qt_verify PROPERTIES OUTPUT_NAME "cute_qt_verify")
