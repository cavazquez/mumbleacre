// SPDX-License-Identifier: GPL-3.0
/*
 * Register the MumbleACRE frequency plan through ACRE's public API.
 *
 * This function must run on every client and the server before a mission hands
 * out radios. It only creates preset data; selecting a preset for a local
 * player belongs to MumbleACRE_ACRE_fnc_setupMission.
 */

if (missionNamespace getVariable ["MumbleACRE_ACRE_presetsConfigured", false]) exitWith {true};

private _srLabels = ["PLT", "SQD1", "SQD2", "SQD3", "FIRE1", "FIRE2", "CAS", "SPARE1", "SPARE2"];
private _lrLabels = ["BN", "BN TAC", "AVN", "ARTY", "LOG", "SPARE1", "SPARE2", "SPARE3", "SPARE4"];

// [side key, PRC-152 frequencies, PRC-117F frequencies]. Every value comes
// from the former MumbleACRE presets and remains in the 30–512 MHz capability of
// both selected ACRE radios.
private _plans = [
    ["west", [41, 42, 43, 44, 45, 46, 47, 48, 49], [51, 52, 53, 54, 55, 56, 57, 58, 59]],
    ["east", [61, 62, 63, 64, 65, 66, 67, 68, 69], [71, 72, 73, 74, 75, 76, 77, 78, 79]],
    ["independent", [81, 82, 83, 84, 85, 86, 87, 88, 89], [91, 92, 93, 94, 95, 96, 97, 98, 99]]
];

private _configureRadio = {
    params ["_radioClass", "_presetName", "_frequencies", "_labels", "_labelField", "_power", ["_setActive", false]];

    if !( [_radioClass, "default", _presetName] call acre_api_fnc_copyPreset ) exitWith {false};

    private _ok = true;
    {
        private _channel = _forEachIndex + 1;
        private _frequency = _x;
        private _label = _labels select _forEachIndex;

        _ok = _ok && ([_radioClass, _presetName, _channel, "frequencyTX", _frequency] call acre_api_fnc_setPresetChannelField);
        _ok = _ok && ([_radioClass, _presetName, _channel, "frequencyRX", _frequency] call acre_api_fnc_setPresetChannelField);
        _ok = _ok && ([_radioClass, _presetName, _channel, _labelField, _label] call acre_api_fnc_setPresetChannelField);
        _ok = _ok && ([_radioClass, _presetName, _channel, "power", _power] call acre_api_fnc_setPresetChannelField);

        if (_setActive) then {
            _ok = _ok && ([_radioClass, _presetName, _channel, "active", true] call acre_api_fnc_setPresetChannelField);
        };
    } forEach _frequencies;

    _ok
};

private _ok = true;
{
    _x params ["_sideKey", "_srFrequencies", "_lrFrequencies"];
    _ok = _ok && (["ACRE_PRC152", format ["mumbleacre_%1_sr", _sideKey], _srFrequencies, _srLabels, "description", 5000] call _configureRadio);
    _ok = _ok && (["ACRE_PRC117F", format ["mumbleacre_%1_lr", _sideKey], _lrFrequencies, _lrLabels, "name", 20000, true] call _configureRadio);
} forEach _plans;

if (!_ok) exitWith {
    diag_log "MumbleACRE ACRE: no se pudieron registrar todos los presets públicos";
    false
};

missionNamespace setVariable ["MumbleACRE_ACRE_presetsConfigured", true];
diag_log "MumbleACRE ACRE: presets west/east/independent registrados";
true
