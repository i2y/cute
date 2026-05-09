# CuteHelpers.cmake - CMake glue for the Cute compiler.
#
# Usage from a downstream CMakeLists.txt:
#
#     find_package(Cute REQUIRED)        # (future) finds cutec + this module
#     find_package(Qt6 REQUIRED COMPONENTS Core Qml)
#
#     cute_add_executable(todomv
#         SOURCES
#             src/todo_item.cute
#             src/main.cute
#         QT_LIBRARIES
#             Qt6::Core
#             Qt6::Qml
#         CUTE_FLAGS
#             --emit cpp
#     )
#
# Status: skeleton. The compile step that turns `.cute` into `.h/.cpp`
# is gated behind the `cutec emit cpp` subcommand. The macro declares
# the public API shape so downstream projects can be wired up.

if(__cute_helpers_included)
    return()
endif()
set(__cute_helpers_included TRUE)

# Locate cutec (compiler binary). For now allow override via -DCUTEC=...
if(NOT DEFINED CUTEC OR CUTEC STREQUAL "")
    find_program(CUTEC NAMES cute HINTS ${CMAKE_SOURCE_DIR}/target/release ${CMAKE_SOURCE_DIR}/target/debug)
endif()

set(CUTE_RUNTIME_INCLUDE_DIRS "${CMAKE_CURRENT_LIST_DIR}/../runtime/cpp" CACHE PATH "Cute runtime header directory")

function(cute_add_executable target)
    cmake_parse_arguments(CA "" "" "SOURCES;QT_LIBRARIES;CUTE_FLAGS" ${ARGN})
    if(NOT CA_SOURCES)
        message(FATAL_ERROR "cute_add_executable: SOURCES is required")
    endif()
    if(NOT CUTEC)
        message(FATAL_ERROR
            "cute_add_executable: cutec not found. Build it first with `cargo build -p cute-cli` "
            "and re-run cmake, or set -DCUTEC=/path/to/cutec.")
    endif()

    set(_generated_dir "${CMAKE_CURRENT_BINARY_DIR}/cute_generated/${target}")
    file(MAKE_DIRECTORY "${_generated_dir}")

    set(_generated_cpp "")
    foreach(src ${CA_SOURCES})
        get_filename_component(stem "${src}" NAME_WE)
        set(out_h "${_generated_dir}/${stem}.h")
        set(out_cpp "${_generated_dir}/${stem}.cpp")
        add_custom_command(
            OUTPUT ${out_h} ${out_cpp}
            COMMAND ${CUTEC} build "${CMAKE_CURRENT_SOURCE_DIR}/${src}"
                            --out-dir "${_generated_dir}"
                            ${CA_CUTE_FLAGS}
            DEPENDS "${CMAKE_CURRENT_SOURCE_DIR}/${src}"
            COMMENT "Compiling ${src} (Cute -> C++)"
            VERBATIM
        )
        list(APPEND _generated_cpp ${out_cpp})
    endforeach()

    add_executable(${target} ${_generated_cpp})
    target_include_directories(${target} PRIVATE ${_generated_dir} ${CUTE_RUNTIME_INCLUDE_DIRS})
    target_compile_features(${target} PRIVATE cxx_std_17)
    if(CA_QT_LIBRARIES)
        target_link_libraries(${target} PRIVATE ${CA_QT_LIBRARIES})
    endif()
endfunction()
