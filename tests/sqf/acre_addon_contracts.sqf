// Headless contracts for the thin ACRE mission addon.
//
// This mocks only documented ACRE API calls. It proves the data submitted by
// MumbleACRE, not the behaviour of ACRE itself; Windows/ACRE remains the E2E gate.

MumbleACRE_ACRE_TEST_failures = 0;
MumbleACRE_ACRE_TEST_cases = 0;

MumbleACRE_ACRE_TEST_fnc_assertEqual = {
    params ["_name", "_actual", "_expected"];
    MumbleACRE_ACRE_TEST_cases = MumbleACRE_ACRE_TEST_cases + 1;
    if !(_actual isEqualTo _expected) then {
        MumbleACRE_ACRE_TEST_failures = MumbleACRE_ACRE_TEST_failures + 1;
        diag_log format [
            "MumbleACRE_ACRE_SQF_FAIL:%1 expected=%2 actual=%3",
            _name,
            _expected,
            _actual
        ];
    };
};

MumbleACRE_ACRE_TEST_copyCalls = [];
MumbleACRE_ACRE_TEST_fieldCalls = [];

acre_api_fnc_copyPreset = {
    MumbleACRE_ACRE_TEST_copyCalls pushBack _this;
    true
};

acre_api_fnc_setPresetChannelField = {
    MumbleACRE_ACRE_TEST_fieldCalls pushBack _this;
    true
};

MumbleACRE_ACRE_fnc_configurePresets = compile preprocessFileLineNumbers "acre-addon/functions/fn_configurePresets.sqf";
// Compile the asynchronous half too. Its API calls require a live ACRE client,
// so their behaviour belongs to the Windows fixture rather than this mock.
MumbleACRE_ACRE_fnc_setupMission = compile preprocessFileLineNumbers "acre-addon/functions/fn_setupMission.sqf";

["configure returns success", [] call MumbleACRE_ACRE_fnc_configurePresets, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["six side/radio presets copied", count MumbleACRE_ACRE_TEST_copyCalls, 6] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["all public field writes", count MumbleACRE_ACRE_TEST_fieldCalls, 243] call MumbleACRE_ACRE_TEST_fnc_assertEqual;

private _expectedCopies = [
    ["ACRE_PRC152", "default", "mumbleacre_west_sr"],
    ["ACRE_PRC117F", "default", "mumbleacre_west_lr"],
    ["ACRE_PRC152", "default", "mumbleacre_east_sr"],
    ["ACRE_PRC117F", "default", "mumbleacre_east_lr"],
    ["ACRE_PRC152", "default", "mumbleacre_independent_sr"],
    ["ACRE_PRC117F", "default", "mumbleacre_independent_lr"]
];
{
    [format ["copy %1", _x], _x in MumbleACRE_ACRE_TEST_copyCalls, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
} forEach _expectedCopies;

MumbleACRE_ACRE_TEST_fnc_hasField = {
    params ["_radio", "_preset", "_channel", "_field", "_value"];
    (MumbleACRE_ACRE_TEST_fieldCalls findIf {
        _x isEqualTo [_radio, _preset, _channel, _field, _value]
    }) >= 0
};

// Boundary channels cover each retained side plan, radio-specific label field,
// TX/RX, power, and PRC-117F active state.
["west SR C1 TX", ["ACRE_PRC152", "mumbleacre_west_sr", 1, "frequencyTX", 41] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["west SR C1 RX", ["ACRE_PRC152", "mumbleacre_west_sr", 1, "frequencyRX", 41] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["west SR label", ["ACRE_PRC152", "mumbleacre_west_sr", 1, "description", "PLT"] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["west SR power", ["ACRE_PRC152", "mumbleacre_west_sr", 1, "power", 5000] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["east SR C9", ["ACRE_PRC152", "mumbleacre_east_sr", 9, "frequencyRX", 69] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["east SR label C9", ["ACRE_PRC152", "mumbleacre_east_sr", 9, "description", "SPARE2"] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["independent LR C1", ["ACRE_PRC117F", "mumbleacre_independent_lr", 1, "frequencyTX", 91] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["independent LR C9", ["ACRE_PRC117F", "mumbleacre_independent_lr", 9, "frequencyRX", 99] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["independent LR label", ["ACRE_PRC117F", "mumbleacre_independent_lr", 1, "name", "BN"] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["independent LR power", ["ACRE_PRC117F", "mumbleacre_independent_lr", 1, "power", 20000] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["independent LR active", ["ACRE_PRC117F", "mumbleacre_independent_lr", 1, "active", true] call MumbleACRE_ACRE_TEST_fnc_hasField, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;

private _stereoWrites = MumbleACRE_ACRE_TEST_fieldCalls select {(_x select 3) isEqualTo "stereo"};
["no parallel stereo setting", _stereoWrites, []] call MumbleACRE_ACRE_TEST_fnc_assertEqual;

private _copiesBeforeIdempotentCall = count MumbleACRE_ACRE_TEST_copyCalls;
private _fieldsBeforeIdempotentCall = count MumbleACRE_ACRE_TEST_fieldCalls;
["configure is idempotent", [] call MumbleACRE_ACRE_fnc_configurePresets, true] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["idempotent copy count", count MumbleACRE_ACRE_TEST_copyCalls, _copiesBeforeIdempotentCall] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["idempotent field count", count MumbleACRE_ACRE_TEST_fieldCalls, _fieldsBeforeIdempotentCall] call MumbleACRE_ACRE_TEST_fnc_assertEqual;

private _realCopyPreset = acre_api_fnc_copyPreset;
acre_api_fnc_copyPreset = {false};
missionNamespace setVariable ["MumbleACRE_ACRE_presetsConfigured", false];
["copy failure is fail-closed", [] call MumbleACRE_ACRE_fnc_configurePresets, false] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
["failed setup is not marked configured", missionNamespace getVariable ["MumbleACRE_ACRE_presetsConfigured", false], false] call MumbleACRE_ACRE_TEST_fnc_assertEqual;
acre_api_fnc_copyPreset = _realCopyPreset;

if (MumbleACRE_ACRE_TEST_failures isEqualTo 0) then {
    diag_log format ["MumbleACRE_ACRE_SQF_PASS:%1 cases", MumbleACRE_ACRE_TEST_cases];
} else {
    diag_log format ["MumbleACRE_ACRE_SQF_FAIL:%1 of %2 cases", MumbleACRE_ACRE_TEST_failures, MumbleACRE_ACRE_TEST_cases];
};
