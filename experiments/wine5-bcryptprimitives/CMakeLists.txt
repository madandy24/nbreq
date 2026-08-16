cmake_minimum_required(VERSION 3.15)

project(nbreq_wine5_bcryptprimitives LANGUAGES C)

add_library(bcryptprimitives SHARED
    bcryptprimitives.c
    bcryptprimitives.def
)

target_compile_definitions(bcryptprimitives PRIVATE WIN32_LEAN_AND_MEAN)
target_link_libraries(bcryptprimitives PRIVATE bcrypt)

if(MSVC)
    target_compile_options(bcryptprimitives PRIVATE /W4 /WX /GS-)
    target_link_options(bcryptprimitives PRIVATE /NOENTRY /NODEFAULTLIB)
endif()
