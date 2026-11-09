#!/usr/bin/env bash
# Запуск remember-me-server в фоне, переживает закрытие SSH-сессии.
# Использование на сервере: ./start-server.sh [аргументы бинарника]

set -euo pipefail

BIN="${BIN:-./remember-me-server}"
LOG="${LOG:-./remember-me-server.log}"
PID_FILE="${PID_FILE:-./remember-me-server.pid}"

if [[ ! -x "$BIN" ]]; then
    echo "Ошибка: $BIN не найден или не исполняемый (chmod +x $BIN)" >&2
    exit 1
fi

# Не запускать второй экземпляр
if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "Сервер уже запущен, PID $(cat "$PID_FILE")"
    exit 0
fi

nohup "$BIN" "$@" >>"$LOG" 2>&1 &
PID=$!
disown "$PID"

echo "$PID" > "$PID_FILE"
echo "remember-me-server запущен, PID $PID"
echo "Логи: tail -f $LOG"
echo "Остановить: kill \$(cat $PID_FILE)"
