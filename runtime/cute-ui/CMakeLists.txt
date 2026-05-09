cmake_minimum_required(VERSION 3.20)

project(cute_ui
    VERSION 0.1.0
    DESCRIPTION "Cute language GPU-accelerated UI runtime (Qt Canvas Painter)"
    LANGUAGES CXX
)

# QtCanvasPainter is a Qt 6.11 Tech Preview module; pinning the minimum
# version here surfaces an actionable error on older Qt installations.
# GuiPrivate is needed to make <rhi/qrhi.h> resolvable.
find_package(Qt6 6.11 COMPONENTS Core Gui GuiPrivate Svg CanvasPainter REQUIRED)

include(FetchContent)
FetchContent_Declare(yoga
    GIT_REPOSITORY https://github.com/facebook/yoga.git
    GIT_TAG v3.2.1
    GIT_SHALLOW TRUE
)
FetchContent_MakeAvailable(yoga)

set(CMAKE_AUTOMOC ON)
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

add_library(cute_ui STATIC
    src/version.cpp
    src/component.cpp
    src/element.cpp
    src/style.cpp
    src/window.cpp
    src/app.cpp
    # AUTOMOC needs to scan headers that hold Q_OBJECT, so they're listed as
    # sources too (modern Qt6 idiom).
    include/cute/ui/component.hpp
    include/cute/ui/app.hpp
    include/cute/ui/window.hpp
)

target_include_directories(cute_ui
    PUBLIC
        $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
        $<INSTALL_INTERFACE:include>
)

target_compile_features(cute_ui PUBLIC cxx_std_17)

target_link_libraries(cute_ui
    PUBLIC
        Qt6::Core
        Qt6::Gui
        Qt6::GuiPrivate
        Qt6::Svg
        Qt6::CanvasPainter
    PRIVATE
        # yogacore symbols are merged into libcute_ui.a by the POST_BUILD
        # libtool step below; the BUILD_INTERFACE link is just for include
        # paths and link order during this project's own build.
        $<BUILD_INTERFACE:yogacore>
)

# Merge yogacore.a into libcute_ui.a so downstream consumers don't need a
# yoga::yogacore imported target. macOS only for now; Linux (ar M scripts)
# and Windows (lib /OUT) need the same treatment when those platforms come
# online.
if(APPLE)
    add_custom_command(TARGET cute_ui POST_BUILD
        COMMAND libtool -static -o $<TARGET_FILE:cute_ui>
                $<TARGET_FILE:cute_ui>
                $<TARGET_FILE:yogacore>
        COMMENT "Bundling yogacore.a into libcute_ui.a"
        VERBATIM
    )
endif()

add_library(cute_ui::cute_ui ALIAS cute_ui)

option(CUTE_UI_BUILD_EXAMPLES "Build cute_ui examples" ON)
if(CUTE_UI_BUILD_EXAMPLES)
    add_subdirectory(examples)
endif()

include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

install(TARGETS cute_ui
    EXPORT CuteUITargets
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}
    LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}
)
install(DIRECTORY include/cute DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
install(EXPORT CuteUITargets
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/CuteUI
    NAMESPACE cute_ui::
    FILE CuteUITargets.cmake
)

configure_package_config_file(
    cmake/CuteUIConfig.cmake.in
    ${CMAKE_CURRENT_BINARY_DIR}/CuteUIConfig.cmake
    INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/CuteUI
)
write_basic_package_version_file(
    ${CMAKE_CURRENT_BINARY_DIR}/CuteUIConfigVersion.cmake
    VERSION ${PROJECT_VERSION}
    COMPATIBILITY SameMajorVersion
)
install(FILES
    ${CMAKE_CURRENT_BINARY_DIR}/CuteUIConfig.cmake
    ${CMAKE_CURRENT_BINARY_DIR}/CuteUIConfigVersion.cmake
    DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/CuteUI
)
