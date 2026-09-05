#!/usr/bin/env bash
# verify-acre-addon.sh — Static contract for the thin ACRE mission addon.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ADDON="$ROOT/acre-addon"
FUNCTIONS="$ADDON/functions"
MISSION="$ROOT/missions/mumbleacre_smoke.VR"
DOC="$ROOT/docs/acre-mission-integration.md"
ERR=0

red() { printf '\033[0;31m%s\033[0m\n' "$*"; }
grn() { printf '\033[0;32m%s\033[0m\n' "$*"; }
fail() { red "FAIL: $*"; ERR=$((ERR + 1)); }
ok() { grn "OK:   $*"; }

require_file() {
    local file="$1"
    if [[ -f "$file" ]]; then
        ok "$(realpath --relative-to="$ROOT" "$file")"
    else
        fail "Falta archivo: $file"
    fi
}

require_text() {
    local file="$1"
    local text="$2"
    local label="$3"
    if rg -Fq -- "$text" "$file"; then
        ok "$label"
    else
        fail "$label (no se encontró $text en $file)"
    fi
}

for file in \
    "$ADDON/\$PBOPREFIX\$" \
    "$ADDON/config.cpp" \
    "$ADDON/CfgFunctions.hpp" \
    "$FUNCTIONS/fn_configurePresets.sqf" \
    "$FUNCTIONS/fn_setupMission.sqf" \
    "$MISSION/mission.sqm" \
    "$MISSION/description.ext" \
    "$MISSION/init.sqf" \
    "$MISSION/initPlayerLocal.sqf" \
    "$MISSION/onPlayerRespawn.sqf" \
    "$DOC"; do
    require_file "$file"
done

if [[ ! -d "$ADDON" || ! -d "$MISSION" ]]; then
    fail "No se puede verificar el addon o la fixture ausente"
else
    echo
    echo "── Dependencias y superficie del addon ──"
    require_text "$ADDON/\$PBOPREFIX\$" 'mumbleacre\addons\mumbleacre_acre' "PBO prefix candidato"
    for dependency in cba_main acre_api acre_sys_prc152 acre_sys_prc117f; do
        require_text "$ADDON/config.cpp" "\"$dependency\"" "requiredAddons contiene $dependency"
    done
    for function in configurePresets setupMission; do
        require_text "$ADDON/CfgFunctions.hpp" "class $function {}" "CfgFunctions registra $function"
        require_file "$FUNCTIONS/fn_${function}.sqf"
    done

    for forbidden in addKeybind callExtension Extended_PreInit_EventHandlers Extended_PostInit_EventHandlers CfgVehicles CfgWeapons CfgSounds CfgRscTitles MumbleACRE_fnc_; do
        if rg -nF -- "$forbidden" "$ADDON" >/dev/null; then
            fail "El addon fino no puede declarar $forbidden"
        else
            ok "No declara $forbidden"
        fi
    done
    for forbidden in acre_sys_ acre_core_fnc; do
        if rg -nF -- "$forbidden" "$FUNCTIONS" >/dev/null; then
            fail "Las funciones no pueden usar API privada $forbidden"
        else
            ok "Funciones sin API privada $forbidden"
        fi
    done

    echo
    echo "── Plan ACRE público ──"
    for api in acre_api_fnc_copyPreset acre_api_fnc_setPresetChannelField acre_api_fnc_setPreset acre_api_fnc_isInitialized acre_api_fnc_getRadioByType acre_api_fnc_setRadioChannel; do
        if rg -nF -- "$api" "$FUNCTIONS" >/dev/null; then
            ok "Usa $api"
        else
            fail "Falta llamada pública $api"
        fi
    done
    require_text "$FUNCTIONS/fn_configurePresets.sqf" '["west", [41, 42, 43, 44, 45, 46, 47, 48, 49], [51, 52, 53, 54, 55, 56, 57, 58, 59]]' "Plan WEST completo"
    require_text "$FUNCTIONS/fn_configurePresets.sqf" '["east", [61, 62, 63, 64, 65, 66, 67, 68, 69], [71, 72, 73, 74, 75, 76, 77, 78, 79]]' "Plan EAST completo"
    require_text "$FUNCTIONS/fn_configurePresets.sqf" '["independent", [81, 82, 83, 84, 85, 86, 87, 88, 89], [91, 92, 93, 94, 95, 96, 97, 98, 99]]' "Plan INDEPENDENT completo"
    require_text "$FUNCTIONS/fn_configurePresets.sqf" '"description", 5000' "PRC-152 conserva 5 W y descripción"
    require_text "$FUNCTIONS/fn_configurePresets.sqf" '"name", 20000, true' "PRC-117F conserva 20 W y activo"
    require_text "$FUNCTIONS/fn_setupMission.sqf" '[_unit] call acre_api_fnc_isInitialized' "Espera IDs ACRE antes del canal"

    echo
    echo "── Fixture Eden ──"
    for dependency in cba_main acre_main acre_api acre_sys_prc152 acre_sys_prc117f mumbleacre_acre; do
        require_text "$MISSION/mission.sqm" "\"$dependency\"" "Fixture declara $dependency"
    done
    playable_count=$(rg -c 'isPlayable=1' "$MISSION/mission.sqm" | awk -F: '{total += $NF} END {print total + 0}')
    if [[ "$playable_count" -eq 2 ]]; then
        ok "Fixture declara exactamente dos slots playables"
    else
        fail "Fixture debe declarar dos slots playables (actual: $playable_count)"
    fi
    require_text "$MISSION/init.sqf" 'MumbleACRE_ACRE_fnc_configurePresets' "init.sqf registra presets en todos los nodos"
    for file in initPlayerLocal.sqf onPlayerRespawn.sqf; do
        require_text "$MISSION/$file" 'MumbleACRE_ACRE_fnc_setupMission' "$file reconfigura la unidad local"
        require_text "$MISSION/$file" 'ACRE_PRC152' "$file entrega PRC-152 ACRE"
        require_text "$MISSION/$file" 'ACRE_PRC117F' "$file entrega PRC-117F ACRE"
    done
    if rg -nF 'MumbleACRE_R' "$MISSION" >/dev/null; then
        fail "La fixture ACRE no puede incluir radios MumbleACRE legadas"
    else
        ok "Fixture sin radios MumbleACRE legadas"
    fi

    echo
    echo "── Higiene SQF ──"
    while IFS= read -r -d '' sqf; do
        opens=$( { grep -o '{' "$sqf" || true; } | wc -l | tr -d ' ')
        closes=$( { grep -o '}' "$sqf" || true; } | wc -l | tr -d ' ')
        if [[ "$opens" -ne "$closes" ]]; then
            fail "Llaves desbalanceadas: $sqf ($opens/$closes)"
        fi
        if rg -n '[[:blank:]]+$' "$sqf" >/dev/null; then
            fail "Whitespace final: $sqf"
        fi
    done < <(find "$ADDON" "$MISSION" -name '*.sqf' -print0 | sort -z)
    ok "Llaves y whitespace SQF verificados"
fi

echo
if [[ $ERR -eq 0 ]]; then
    grn "verify-acre-addon: PASS"
    exit 0
fi

red "verify-acre-addon: $ERR error(es)"
exit 1
