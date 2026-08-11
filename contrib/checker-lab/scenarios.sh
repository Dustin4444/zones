# Bridge scenarios exercised by the checker lab.

checker_json() {
    "$ZONE_BIN" checker inspect --checker.database-path "$CHECKER_DB" --json
}
wait_for_checker() {
    local l1_target="$1"
    local l2_target="$2"
    local i state imported verified finding
    for ((i = 0; i < 120; i++)); do
        assert_running "$ZONE_PID_FILE" "Zone" "$ZONE_LOG"
        if state="$(checker_json 2>/dev/null)"; then
            imported="$(jq -r '.importedTempoTip.number' <<<"$state")"
            verified="$(jq -r '.verifiedZoneTip.number' <<<"$state")"
            finding="$(jq -r '.activeFinding' <<<"$state")"
            if [[ "$finding" == "true" ]]; then
                say "Checker recorded a finding"
                printf '%s\n' "$state"
                return
            fi
            if ((imported >= l1_target && verified >= l2_target)); then
                say "Checker verified the triggered transition"
                printf '%s\n' "$state"
                return
            fi
        fi
        sleep 1
    done
    die "timed out waiting for checker progress; inspect $ZONE_LOG"
}

export_trigger_environment() {
    export L1_RPC_URL="$L1_HTTP_URL"
    export L1_PORTAL_ADDRESS
    L1_PORTAL_ADDRESS="$(zone_metadata portal)"
    export ZONE_RPC_URL="$ZONE_HTTP_URL"
    export PRIVATE_KEY="$DEV_KEY"
    export ADMIN_KEY="$DEV_KEY"
}

trigger() {
    local scenario="${1:-}"
    assert_running "$L1_PID_FILE" "Tempo L1" "$L1_LOG"
    assert_running "$ZONE_PID_FILE" "Zone" "$ZONE_LOG"
    export_trigger_environment
    case "$scenario" in
        token)
            local salt suffix output token
            suffix="$(date +%s)"
            salt="$(cast keccak "checker-lab-$suffix-$$")"
            output="$(just --justfile "$ZONES_ROOT/Justfile" create-token \
                "Checker $suffix" "CHK" "$salt")"
            printf '%s\n' "$output"
            token="$(awk '/Address:/ {print $2; exit}' <<<"$output")"
            [[ "$token" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "could not parse token address"
            just --justfile "$ZONES_ROOT/Justfile" enable-token "$token"
            ;;
        deposit)
            just --justfile "$ZONES_ROOT/Justfile" max-approve-portal "$PATH_USD"
            just --justfile "$ZONES_ROOT/Justfile" send-deposit \
                1000000 "$DEV_ADDRESS" \
                0x0000000000000000000000000000000000000000000000000000000000000000 \
                "$PATH_USD" "$ZONE_HTTP_URL"
            ;;
        withdrawal)
            just --justfile "$ZONES_ROOT/Justfile" max-approve-portal "$PATH_USD"
            just --justfile "$ZONES_ROOT/Justfile" send-deposit \
                1000000 "$DEV_ADDRESS" \
                0x0000000000000000000000000000000000000000000000000000000000000000 \
                "$PATH_USD" "$ZONE_HTTP_URL"
            just --justfile "$ZONES_ROOT/Justfile" max-approve-outbox "$PATH_USD" "$ZONE_HTTP_URL"
            just --justfile "$ZONES_ROOT/Justfile" send-withdrawal \
                100000 "$DEV_ADDRESS" "$PATH_USD" \
                0x0000000000000000000000000000000000000000000000000000000000000000 \
                0 "$DEV_ADDRESS" 0x 0x "$ZONE_HTTP_URL"
            ;;
        *) die "trigger expects one of: token, deposit, withdrawal" ;;
    esac
    wait_for_checker \
        "$(cast block-number --rpc-url "$L1_HTTP_URL")" \
        "$(cast block-number --rpc-url "$ZONE_HTTP_URL")"
}
