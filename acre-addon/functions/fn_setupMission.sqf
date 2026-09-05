// SPDX-License-Identifier: GPL-3.0
/*
 * Select the local ACRE defaults for a faction and, after the radio IDs exist,
 * select an initial channel on the locally carried PRC-152 / PRC-117F.
 *
 * Arguments:
 * 0: optional side key: "west", "east", or "independent". Empty selects the
 *    local player's side. Other sides use west and emit a diagnostic.
 * 1: optional initial channel, 1 through 9 (default 1).
 *
 * The default preset must be selected before a mission adds a base radio.
 * ACRE's isInitialized API is therefore used for the later, ID-dependent
 * active-channel operation, never as a reason to mutate private ACRE state.
 */

params [
    ["_requestedSide", "", [""]],
    ["_channel", 1, [0]]
];

if (!hasInterface) exitWith {false};

if (isNull player) exitWith {
    [{!isNull player}, {
        params ["_requestedSide", "_channel"];
        [_requestedSide, _channel] call MumbleACRE_ACRE_fnc_setupMission;
    }, [_requestedSide, _channel]] call CBA_fnc_waitUntilAndExecute;
    true
};

if !([] call MumbleACRE_ACRE_fnc_configurePresets) exitWith {false};

if (_channel < 1 || {_channel > 9}) exitWith {
    diag_log format ["MumbleACRE ACRE: canal inicial inválido %1", _channel];
    false
};

private _sideKey = _requestedSide;
if (_sideKey isEqualTo "") then {
    switch (side group player) do {
        case west: {_sideKey = "west"};
        case east: {_sideKey = "east"};
        case independent: {_sideKey = "independent"};
        default {
            _sideKey = "west";
            diag_log "MumbleACRE ACRE: bando no previsto; se usa el preset west";
        };
    };
};

if !(_sideKey in ["west", "east", "independent"]) exitWith {
    diag_log format ["MumbleACRE ACRE: preset de bando inválido %1", _sideKey];
    false
};

private _srPreset = format ["mumbleacre_%1_sr", _sideKey];
private _lrPreset = format ["mumbleacre_%1_lr", _sideKey];
["ACRE_PRC152", _srPreset] call acre_api_fnc_setPreset;
["ACRE_PRC117F", _lrPreset] call acre_api_fnc_setPreset;

private _unit = player;
private _deadline = diag_tickTime + 30;
[{
    params ["_unit", "_deadline"];

    if (diag_tickTime >= _deadline || {!(_unit isEqualTo player)}) exitWith {true};
    if !([_unit] call acre_api_fnc_isInitialized) exitWith {false};
    !isNil { ["ACRE_PRC152", _unit] call acre_api_fnc_getRadioByType }
}, {
    params ["_unit", "_deadline", "_channel"];

    if (!(_unit isEqualTo player)) exitWith {
        diag_log "MumbleACRE ACRE: se canceló el setup de radio de una unidad anterior";
    };
    if (diag_tickTime >= _deadline || {!([_unit] call acre_api_fnc_isInitialized)}) exitWith {
        diag_log "MumbleACRE ACRE: ACRE no inicializó la radio dentro de 30 segundos";
    };

    private _srRadio = ["ACRE_PRC152", _unit] call acre_api_fnc_getRadioByType;
    if (!isNil "_srRadio") then {
        [_srRadio, _channel] call acre_api_fnc_setRadioChannel;
    };

    private _lrRadio = ["ACRE_PRC117F", _unit] call acre_api_fnc_getRadioByType;
    if (!isNil "_lrRadio") then {
        [_lrRadio, _channel] call acre_api_fnc_setRadioChannel;
    };

    diag_log format ["MumbleACRE ACRE: %1 listo en canal %2", _unit, _channel];
}, [_unit, _deadline, _channel]] call CBA_fnc_waitUntilAndExecute;

true
