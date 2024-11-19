@ECHO OFF

SET "BUILD_DIR=.\build\msvc"

IF NOT EXIST "%BUILD_DIR%" (
  mkdir "%BUILD_DIR%"
)

IF NOT EXIST "%BUILD_DIR%\foo" (
  mkdir "%BUILD_DIR%\foo"
)

cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\bar.obj" /ifcOutput "%BUILD_DIR%\bar.ifc" /interface /Tp bar.cppm /scanDependencies "%BUILD_DIR%\bar.ddi"
cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\bar.obj" /ifcOutput "%BUILD_DIR%\bar.ifc" /interface /Tp bar.cppm

cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\foo\part1.obj" /ifcOutput "%BUILD_DIR%\foo\part1.ifc" /interface /Tp foo\part1.cppm /scanDependencies "%BUILD_DIR%\foo\part1.ddi"
cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\foo\part1.obj" /ifcOutput "%BUILD_DIR%\foo\part1.ifc" /interface /Tp foo\part1.cppm

cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\foo\part2.obj" /ifcOutput "%BUILD_DIR%\foo\part2.ifc" /interface /Tp foo\part2.cppm /scanDependencies "%BUILD_DIR%\foo\part2.ddi"
cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\foo\part2.obj" /ifcOutput "%BUILD_DIR%\foo\part2.ifc" /interface /Tp foo\part2.cppm

cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\foo.obj" /ifcOutput "%BUILD_DIR%\foo.ifc" /interface /Tp foo.cppm /ifcSearchDir "%BUILD_DIR%" /ifcSearchDir "%BUILD_DIR%\foo" /reference "foo.baz:part1.qux"="%BUILD_DIR%\foo\part1.ifc" /reference "foo.baz:part2.qux"="%BUILD_DIR%\foo\part2.ifc" /scanDependencies "%BUILD_DIR%\foo.ddi"
cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\foo.obj" /ifcOutput "%BUILD_DIR%\foo.ifc" /interface /Tp foo.cppm /ifcSearchDir "%BUILD_DIR%" /ifcSearchDir "%BUILD_DIR%\foo" /reference "foo.baz:part1.qux"="%BUILD_DIR%\foo\part1.ifc" /reference "foo.baz:part2.qux"="%BUILD_DIR%\foo\part2.ifc"

cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\main.obj" /Tp main.cpp /ifcSearchDir "%BUILD_DIR%" /ifcSearchDir "%BUILD_DIR%\foo" /reference "foo.baz:part1.qux"="%BUILD_DIR%\foo\part1.ifc" /reference "foo.baz:part2.qux"="%BUILD_DIR%\foo\part2.ifc" /reference "foo.baz"="%BUILD_DIR%\foo.ifc" /scanDependencies "%BUILD_DIR%\main.ddi"
cl /EHsc /nologo /std:c++20 /c /Fo: "%BUILD_DIR%\main.obj" /Tp main.cpp /ifcSearchDir "%BUILD_DIR%" /ifcSearchDir "%BUILD_DIR%\foo" /reference "foo.baz:part1.qux"="%BUILD_DIR%\foo\part1.ifc" /reference "foo.baz:part2.qux"="%BUILD_DIR%\foo\part2.ifc" /reference "foo.baz"="%BUILD_DIR%\foo.ifc"

cl /nologo "%BUILD_DIR%\main.obj" "%BUILD_DIR%\foo.obj" "%BUILD_DIR%\bar.obj" /link /out:"%BUILD_DIR%\main.exe"
