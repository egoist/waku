#!/bin/sh
# Model Codex's process-owned writer, including persistence during EOF shutdown.
fixture_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
owns_fork=false
thread_id=thread-original

while IFS= read -r request; do
    id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
    case "$request" in
        *'"method":"initialize"'*)
            printf '{"id":%s,"result":{}}\n' "$id"
            ;;
        *'"method":"initialized"'*) ;;
        *'"method":"thread/resume"'*)
            case "$request" in
                *'"threadId":"thread-fork"'*)
                    if [ -d "$fixture_dir/fork.writer" ]; then
                        printf '{"id":%s,"error":{"message":"thread thread-fork already has an active writer"}}\n' "$id"
                        continue
                    fi
                    thread_id=thread-fork
                    turns='[{"id":"turn-1"}]'
                    ;;
                *) turns='[{"id":"turn-1"},{"id":"turn-2"}]' ;;
            esac
            printf '{"id":%s,"result":{"thread":{"id":"%s","turns":%s}}}\n' "$id" "$thread_id" "$turns"
            ;;
        *'"method":"thread/fork"'*)
            case "$request" in
                *'"lastTurnId":"turn-1"'*) ;;
                *)
                    printf '{"id":%s,"error":{"message":"Cannot fork this turn"}}\n' "$id"
                    continue
                    ;;
            esac
            if ! mkdir "$fixture_dir/fork.writer"; then
                exit 1
            fi
            owns_fork=true
            printf '{"id":%s,"result":{"thread":{"id":"thread-fork"}}}\n' "$id"
            ;;
        *'"method":"turn/start"'*)
            printf '{"id":%s,"result":{"turn":{"id":"turn-new"}}}\n' "$id"
            printf '{"method":"turn/started","params":{"threadId":"%s","turn":{"id":"turn-new"}}}\n' "$thread_id"
            printf '{"method":"item/agentMessage/delta","params":{"threadId":"%s","delta":"OK"}}\n' "$thread_id"
            printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"turn-new","status":"completed"}}}\n' "$thread_id"
            ;;
        *) printf '{"id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id" ;;
    esac
done

# Returning the fork RPC result is earlier than releasing its writer. Killing
# the process instead of allowing EOF shutdown would also skip this flush.
if [ "$owns_fork" = true ]; then
    sleep 0.1
    rmdir "$fixture_dir/fork.writer"
fi
