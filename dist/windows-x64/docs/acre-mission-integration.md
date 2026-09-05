# Integración de misión con ACRE2

El addon fino `mumbleacre_acre.pbo` usa exclusivamente la API pública de ACRE2. No
declara radios, PTT, HUD, diálogos, propagación, inventario propio ni extensión.
ACRE2 sigue siendo dueño de esas funciones; MumbleACRE sólo conserva el plan de
redes de la misión para la fixture ACRE.

## Radios elegidas

| Uso de misión | Radio ACRE2 | Potencia por canal | Motivo |
|---|---|---:|---|
| Red táctica de todos los jugadores | `ACRE_PRC152` | 5 W | Radio portátil; representa las nueve redes tácticas. |
| Red de mando / RTO | `ACRE_PRC117F` | 20 W | Radio de mochila; representa las nueve redes de mando. |

`SR` y `LR` son nombres de rol de la migración, no bandas artificiales. Las dos
radios cubren todas las frecuencias conservadas, de 41 a 99 MHz. El stereo no
se copia como un campo propio: ACRE2 controla su enrutamiento de audio y el
addon no inventa un estado paralelo. La fixture deja la configuración nativa
de ACRE para stereo y volumen.

## Plan de frecuencias conservado

Cada frecuencia del preset anterior se conserva sin transformación. `TX` y
`RX` tienen el mismo valor, sin cifrado añadido por MumbleACRE. En `PRC-117F`, los
canales se marcan además como activos; en `PRC-152`, ACRE no expone ese campo
por canal y conserva el estado nativo.

| Bando | PRC-152, C1–C9 | PRC-117F, C1–C9 |
|---|---|---|
| WEST | 41 PLT; 42 SQD1; 43 SQD2; 44 SQD3; 45 FIRE1; 46 FIRE2; 47 CAS; 48 SPARE1; 49 SPARE2 | 51 BN; 52 BN TAC; 53 AVN; 54 ARTY; 55 LOG; 56 SPARE1; 57 SPARE2; 58 SPARE3; 59 SPARE4 |
| EAST | 61 PLT; 62 SQD1; 63 SQD2; 64 SQD3; 65 FIRE1; 66 FIRE2; 67 CAS; 68 SPARE1; 69 SPARE2 | 71 BN; 72 BN TAC; 73 AVN; 74 ARTY; 75 LOG; 76 SPARE1; 77 SPARE2; 78 SPARE3; 79 SPARE4 |
| INDEPENDENT | 81 PLT; 82 SQD1; 83 SQD2; 84 SQD3; 85 FIRE1; 86 FIRE2; 87 CAS; 88 SPARE1; 89 SPARE2 | 91 BN; 92 BN TAC; 93 AVN; 94 ARTY; 95 LOG; 96 SPARE1; 97 SPARE2; 98 SPARE3; 99 SPARE4 |

## Uso en una misión

La misión debe ejecutar la definición de presets en todos los nodos y elegir
el preset sólo en cada cliente local. El orden importa: `setPreset` debe ocurrir
antes de entregar la clase base de radio; ACRE2 reemplaza después esa clase por
un ID único.

```sqf
// init.sqf — servidor y todos los clientes
[] call MumbleACRE_ACRE_fnc_configurePresets;

// initPlayerLocal.sqf — sólo el cliente local
[] call MumbleACRE_ACRE_fnc_setupMission;
player addItem "ACRE_PRC152";
player addBackpack "B_AssaultPack_khk";
player addItemToBackpack "ACRE_PRC117F";
```

`MumbleACRE_ACRE_fnc_setupMission` infiere `west`, `east` o `independent` del
jugador y deja ambos equipos en C1. También puede recibir una red explícita:

```sqf
["east", 4] call MumbleACRE_ACRE_fnc_setupMission;
```

Después espera `acre_api_fnc_isInitialized`; recién entonces usa los IDs únicos
con `acre_api_fnc_setRadioChannel`. Tiene un límite de 30 segundos y deja un
único `diag_log` si ACRE no convierte la radio. Eso evita aplicar un canal a una
clase base o dejar un script de espera eterno durante respawn/JIP.

Las únicas llamadas ACRE usadas son públicas:

- `acre_api_fnc_copyPreset`
- `acre_api_fnc_setPresetChannelField`
- `acre_api_fnc_setPreset`
- `acre_api_fnc_isInitialized`
- `acre_api_fnc_getRadioByType`
- `acre_api_fnc_setRadioChannel`

No usar variables del plugin TeamSpeak, `acre_sys_*`, ni IDs de radio fijados
a mano. Tampoco se usa `linkItem`: ACRE2 documenta que las radios se agregan
con `addItem` y luego las convierte a IDs propios.

## Fixture y empaquetado

`missions/mumbleacre_smoke.VR` contiene dos slots WEST, PRC-152 y PRC-117F,
con reposición en respawn. `scripts/build-windows.sh` crea el PBO en
`dist/windows-x64/@mumbleacre/addons/mumbleacre_acre.pbo`.

El addon es opcional para misiones que ya configuran sus radios con ACRE.
La fixture y el addon deben probarse en Arma real en listen, dedicado, JIP y
respawn antes de usarlos en una partida. Los contratos SQF usan mocks de la API
pública y no reemplazan esa prueba.
