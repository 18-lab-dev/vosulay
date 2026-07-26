cmake_minimum_required(VERSION 3.20)
project(vosulay_install NONE)

set(CMAKE_INSTALL_DATAROOTDIR "share" CACHE PATH "Read-only architecture-independent data root")

install(
    FILES extra/com.vosu.vosulay.desktop
    DESTINATION "${CMAKE_INSTALL_DATAROOTDIR}/applications"
)
install(
    FILES assets/branding/vosulay-app-icon.png
    DESTINATION "${CMAKE_INSTALL_DATAROOTDIR}/icons/hicolor/1024x1024/apps"
    RENAME com.vosu.vosulay.png
)
