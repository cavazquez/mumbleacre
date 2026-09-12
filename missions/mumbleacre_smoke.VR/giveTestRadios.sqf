// SPDX-License-Identifier: GPL-3.0
// The fixture waits until ACRE owns the local unit before giving it base radio
// classes. ACRE then converts them to unique IDs, which makes the short-range
// PRC-152 available through the player's configured ACRE radio key.

params ["_unit"];

if (isNull _unit || {!hasInterface}) exitWith {};

[{
    params ["_unit"];
    !isNull _unit && {_unit isEqualTo player} && {[_unit] call acre_api_fnc_isInitialized}
}, {
    params ["_unit"];

    _unit addItem "ACRE_PRC152";
    if ((backpack _unit) isEqualTo "") then {
        _unit addBackpack "B_AssaultPack_khk";
    };
    _unit addItemToBackpack "ACRE_PRC117F";

    [_unit] spawn {
        params ["_unit"];
        sleep 3;

        private _shortRange = ["ACRE_PRC152", _unit] call acre_api_fnc_getRadioByType;
        if (isNil "_shortRange") exitWith {
            diag_log "MumbleACRE Smoke: no se pudo confirmar la PRC-152 ACRE";
            systemChat "MumbleACRE Smoke: no se confirmó la PRC-152; revisa que ACRE2 esté cargado.";
        };

        diag_log "MumbleACRE Smoke: PRC-152 ACRE entregada y lista en canal 1";
        systemChat "MumbleACRE Smoke: PRC-152 ACRE lista en canal 1. Abre la radio con tu tecla configurada de ACRE.";
    };
}, [_unit]] call CBA_fnc_waitUntilAndExecute;
