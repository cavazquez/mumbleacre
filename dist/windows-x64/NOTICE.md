# Proveniencia

MumbleACRE reutiliza y modifica código de [RMTFAR](https://github.com/cavazquez/rmtfar),
GPL-3.0, de sus contribuidores. Fuente local: commit
`f831a67472b72ab6cd48ccda481b95a310e30af5` y su árbol de trabajo al 2026-09-04.

Se conservaron el codec/RPC/estado ACRE, renderer de audio ACRE, preparación de
sonidos, logging, binding ABI de Mumble y la integración mínima de misión con
sus fixtures. Se agregó el runtime MumbleACRE y el transporte de estado entre
pares. No se importaron radios/UI, extensión Arma, bridge, UDP ni instalador
específico del prototipo anterior.

El código derivado se distribuye bajo GPL-3.0 (ver LICENSE). Esto sustituye la
licencia Apache del esqueleto inicial de este repositorio para el producto
combinado; no cambia la licencia de dependencias externas. ACRE2 permanece
externo: no se redistribuyen sus assets ni su plugin TeamSpeak. El proyecto no
es una integración oficial de ACRE2 ni Mumble.
