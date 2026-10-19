cmake_minimum_required(VERSION 3.21)
project(deckardd VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 6.5 REQUIRED COMPONENTS Core DBus Gui Network)
include(GNUInstallDirs)

add_library(deckardcore STATIC
    src/audio.cpp
    src/bootwatchdog.cpp
    src/bridge.cpp
    src/callers.cpp
    src/doze.cpp
    src/images.cpp
    src/ime.cpp
    src/intents.cpp
    src/keelapps.cpp
    src/manage.cpp
    src/mpris.cpp
    src/notifications.cpp
    src/packages.cpp
    src/palette.cpp
    src/permissionmap.cpp
    src/reclaimer.cpp
    src/runtime.cpp
    src/sessionfollower.cpp
    src/settingsapi.cpp
    src/share.cpp
    src/sharedstorage.cpp
    src/validate.cpp
)
target_include_directories(deckardcore PUBLIC src)
target_link_libraries(deckardcore PUBLIC Qt6::Core Qt6::DBus Qt6::Gui Qt6::Network)
target_compile_definitions(deckardcore PUBLIC QT_NO_CAST_FROM_ASCII QT_NO_KEYWORDS PROJECT_VERSION="${PROJECT_VERSION}")

add_executable(deckardd src/main.cpp)
target_link_libraries(deckardd PRIVATE deckardcore)
install(TARGETS deckardd RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(FILES deckardd.service DESTINATION ${CMAKE_INSTALL_PREFIX}/lib/systemd/user)
install(FILES deckardd.service.json DESTINATION ${CMAKE_INSTALL_DATADIR}/marathon-shell/services)

option(BUILD_TESTING "Build the tests" ON)
if(BUILD_TESTING)
    enable_testing()
    find_package(Qt6 REQUIRED COMPONENTS Test)
    add_executable(test_validate tests/test_validate.cpp)
    target_link_libraries(test_validate PRIVATE deckardcore Qt6::Test)
    add_test(NAME validate COMMAND test_validate)
    add_executable(test_bridge tests/test_bridge.cpp)
    target_link_libraries(test_bridge PRIVATE deckardcore Qt6::Test)
    add_test(NAME bridge COMMAND test_bridge)
    add_executable(test_ime tests/test_ime.cpp)
    target_link_libraries(test_ime PRIVATE deckardcore Qt6::Test)
    add_test(NAME ime COMMAND test_ime)
    add_executable(test_intents tests/test_intents.cpp)
    target_link_libraries(test_intents PRIVATE deckardcore Qt6::Test)
    add_test(NAME intents COMMAND test_intents)
    add_executable(test_palette tests/test_palette.cpp)
    target_link_libraries(test_palette PRIVATE deckardcore Qt6::Test)
    target_compile_definitions(test_palette PRIVATE
        DECKARD_PALETTE_XML="${CMAKE_CURRENT_SOURCE_DIR}/../../image/vendor_deckard/overlay/DeckardPalette/res/values/colors.xml")
    add_test(NAME palette COMMAND test_palette)
    add_executable(test_bootwatchdog tests/test_bootwatchdog.cpp)
    target_link_libraries(test_bootwatchdog PRIVATE deckardcore Qt6::Test)
    add_test(NAME bootwatchdog COMMAND test_bootwatchdog)
    add_executable(test_share tests/test_share.cpp)
    target_link_libraries(test_share PRIVATE deckardcore Qt6::Test)
    add_test(NAME share COMMAND test_share)
    add_executable(test_sharedstorage tests/test_sharedstorage.cpp)
    target_link_libraries(test_sharedstorage PRIVATE deckardcore Qt6::Test)
    add_test(NAME sharedstorage COMMAND test_sharedstorage)
    add_test(NAME shared_folders_hook COMMAND python3 -I ${CMAKE_CURRENT_SOURCE_DIR}/../packaging/tests/test-deckard-shared-folders.py
                                                 ${CMAKE_CURRENT_SOURCE_DIR}/../packaging/deckard-shared-folders)
    add_executable(test_reclaimer tests/test_reclaimer.cpp)
    target_link_libraries(test_reclaimer PRIVATE deckardcore Qt6::Test)
    add_test(NAME reclaimer COMMAND test_reclaimer)
    add_executable(test_sessionfollower tests/test_sessionfollower.cpp)
    target_link_libraries(test_sessionfollower PRIVATE deckardcore Qt6::Test)
    add_test(NAME sessionfollower COMMAND test_sessionfollower)
    add_executable(test_packages tests/test_packages.cpp)
    target_link_libraries(test_packages PRIVATE deckardcore Qt6::Test)
    add_test(NAME packages COMMAND test_packages)
    add_executable(test_settingsapi tests/test_settingsapi.cpp)
    target_link_libraries(test_settingsapi PRIVATE deckardcore Qt6::Test)
    add_test(NAME settingsapi COMMAND test_settingsapi)
    add_test(NAME reset_helper COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/../packaging/tests/test-deckard-reset.sh
                                          ${CMAKE_CURRENT_SOURCE_DIR}/../packaging/deckard-reset)
    add_test(NAME overlay_sync COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/../packaging/tests/test-deckard-overlay-sync.sh
                                          ${CMAKE_CURRENT_SOURCE_DIR}/../packaging/deckard-overlay-sync)
    add_executable(test_keelapps tests/test_keelapps.cpp)
    target_link_libraries(test_keelapps PRIVATE deckardcore Qt6::Test)
    add_test(NAME keelapps COMMAND test_keelapps)
endif()
