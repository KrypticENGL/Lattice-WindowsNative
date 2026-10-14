# Regenerates the committed object-file fixtures. Tests use the committed
# files, so this is only needed when the sources change.
#
# Requires:
#   - Visual Studio 2022+ with the C++ x64 tools (found via vswhere)
#   - MinGW-w64 gcc on PATH
#   - rustup target add x86_64-unknown-linux-gnu
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
Push-Location $here
try {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    $vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
    # vcvars64.bat itself calls vswhere.exe by name.
    $env:PATH = "$(Split-Path $vswhere);$env:PATH"

    function Invoke-Cl([string]$arguments) {
        # /GS- and /Zl keep the objects free of security-cookie and default-lib noise.
        cmd /c "`"$vcvars`" >nul && cl /nologo /c /GS- /Zl $arguments"
        if ($LASTEXITCODE) { throw "cl failed: $arguments" }
    }

    # MSVC COFF: CodeView in .debug$S/.debug$T (/Z7), one section per function (/Gy).
    Invoke-Cl '/Od /Gy /Z7 /Fomsvc_z7.obj src\graph.c'
    Invoke-Cl '/Od /Gy /Fomsvc_nodebug.obj src\graph.c'
    Invoke-Cl '/Od /Gy /EHsc /Fomsvc_comdat.obj src\comdat.cpp'
    Invoke-Cl '/Od /Fomsvc_empty.obj src\empty.c'

    # MinGW COFF with DWARF.
    $map = "-fdebug-prefix-map=$here=."
    gcc -c -O0 -g -ffunction-sections -fdata-sections $map -o mingw_dwarf.o src/graph.c
    if ($LASTEXITCODE) { throw 'gcc failed' }
    gcc -c -O0 -ffunction-sections -fdata-sections -o mingw_nodebug.o src/graph.c
    if ($LASTEXITCODE) { throw 'gcc failed' }
    # MinGW C++: vtables, typeinfo, static initialisers, .refptr cells.
    g++ -c -O0 -g -ffunction-sections $map -o mingw_cpp.o src/shapes.cpp
    if ($LASTEXITCODE) { throw 'g++ failed' }

    # ELF with DWARF.
    rustc --crate-type=lib --edition=2021 --target x86_64-unknown-linux-gnu `
        --emit=obj -g -C opt-level=1 -C panic=abort -C codegen-units=1 `
        --remap-path-prefix "$here=." -o elf_dwarf.o src/lib.rs
    if ($LASTEXITCODE) { throw 'rustc failed' }

    Get-ChildItem *.obj, *.o | Select-Object Name, Length
}
finally {
    Pop-Location
}
