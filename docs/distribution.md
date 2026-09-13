# Distribuir MumbleACRE

El código fuente y los archivos de distribución son cosas distintas. El código
se versiona; los resultados de compilación viven en `dist/`, que Git ignora.
Para publicar una versión se adjunta el ZIP generado al lugar de distribución
elegido, pero no se agregan DLL, PBO, bundles ni ZIP al commit de código.

## Generar una release Windows x64

En Ubuntu o WSL, con Rust, el target `x86_64-pc-windows-gnu`, `mingw-w64`,
Python 3.11 o superior y `armake2` instalados, ejecutar:

```bash
./scripts/build-windows.sh
```

El script compila el plugin, crea el PBO del addon y llama a
`scripts/package.py`. También se puede ejecutar este último por separado
cuando la DLL y el PBO ya existan en `dist/windows-x64/`.

La versión procede únicamente de `workspace.package.version` en `Cargo.toml`.
Para una corrección distribuida se incrementa el último dígito antes de
generar el archivo.

## Contenido generado

`dist/windows-x64/` contiene el bundle nativo
`MumbleACRE.mumble_plugin`, el addon `@mumbleacre`, la fixture, las guías, los
scripts de arranque y diagnóstico, y `SHA256SUMS`. El instalable para entregar
es:

```text
MumbleACRE-<version>-windows-x64.zip
```

Al descomprimirlo, todos los archivos quedan bajo una única carpeta con ese
mismo nombre. El usuario instala `MumbleACRE.mumble_plugin` en Mumble y, si va
a usar la fixture, copia `@mumbleacre` y la misión en su instalación de Arma.

`SHA256SUMS` enumera los archivos dentro de la carpeta distribuida, excepto el
propio archivo de hashes. El ZIP no se incluye en esa lista porque se verifica
después de extraerlo.

## Reproducibilidad e integridad

Los dos archivos ZIP se escriben con orden de entradas, compresión y timestamps
fijos. Con DLL, PBO y archivos de origen idénticos, el empaquetado produce el
mismo resultado. La reproducibilidad de la DLL/PBO depende además del
compilador y de las herramientas usadas para construirlos.

Antes de publicar, extraer el ZIP y validar los hashes. En PowerShell, desde la
carpeta extraída:

```powershell
Get-Content .\SHA256SUMS | ForEach-Object {
  $expected, $path = $_ -split '\s+', 2
  $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLower()
  if ($actual -ne $expected) { throw "SHA256 invalido: $path" }
}
```

Una verificación correcta no sustituye `./check.sh`: ese comando prueba el
código y la compilación; el listado de hashes comprueba exactamente los
archivos que recibirán los jugadores.
