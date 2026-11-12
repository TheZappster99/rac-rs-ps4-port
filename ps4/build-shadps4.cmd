@echo off
rem PS4 port: builds the patched shadPS4 used by ps4\emu-run.sh. Source: build-ps4\shadps4\src = github.com/shadps4-emu/shadPS4 at
rem tag 0.21.0 (commit 11557f2e, cloned with submodules), with ps4\shadps4.patch applied (git apply). Built with Visual Studio's
rem clang-cl, CMake and Ninja (the "Desktop development with C++" workload plus its Clang parts), as shadPS4's own Windows CI does.
rem Result: build-ps4\shadps4\src\build\shadps4.exe, which emu-run.sh prefers over the stock release.
call "C:\Program Files\Microsoft Visual Studio\18\Community\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
rem glslang's configure step needs Python 3: the project's own (toolchain\python, from setup.ps1).
set "PY=%~dp0..\toolchain\python"
set "PATH=%PY%;%PATH%"
cd /d "%~dp0..\build-ps4\shadps4\src" || exit /b 1
if not exist build\build.ninja (
	cmake -G Ninja -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=clang-cl -DCMAKE_CXX_COMPILER=clang-cl -DPython3_EXECUTABLE="%PY%\python.exe" || exit /b 1
)
cmake --build build --parallel %NUMBER_OF_PROCESSORS% || exit /b 1
