#!/bin/sh
# usage: measure.sh <label> <seconds>   (BusyBox ash)
LABEL=$1; SECS=${2:-300}
PID=$(pidof zte-agent)
OUT=/tmp/measure-$LABEL
rm -rf $OUT; mkdir -p $OUT
echo "pid=$PID" > $OUT/pid
snap() {
  d=$OUT/$1; mkdir -p $d
  cat /proc/$PID/stat > $d/stat
  cat /proc/$PID/status > $d/status
  cat /proc/loadavg > $d/loadavg
  grep -E '^(cpu |processes|ctxt|btime)' /proc/stat > $d/procstat
  cat /proc/uptime > $d/uptime
  for t in /proc/$PID/task/*; do
    tid=${t##*/}
    echo "$tid $(cat $t/stat)" >> $d/task_stat
    echo "$tid $(cat $t/schedstat)" >> $d/task_schedstat
    echo "$tid $(cat $t/wchan 2>/dev/null)" >> $d/task_wchan
    echo "== $tid" >> $d/task_sched
    grep -E 'sum_exec_runtime|nr_switches|nr_voluntary|nr_involuntary|nr_wakeups' $t/sched >> $d/task_sched 2>/dev/null
  done
  awk 'NR>1 && $2 ~ /:2382$/ {print $2, $3, $4}' /proc/net/tcp > $d/tcp9090
}
TR=/sys/kernel/debug/tracing
[ -d $TR ] || mount -t debugfs none /sys/kernel/debug 2>/dev/null
[ -d $TR ] || TR=/sys/kernel/tracing
[ -d $TR ] || mount -t tracefs none /sys/kernel/tracing 2>/dev/null
TRACE=0
if [ -d $TR/events/sched/sched_process_fork ]; then
  echo 0 > $TR/tracing_on
  echo > $TR/trace
  # Named threads (la-push, hub-ubus, event-bus, ...) no longer have comm "zte-agent", so trace
  # every fork with its TGID and let analyze.py keep the ones whose parent is in the agent.
  # The listener's http-blk threads exit 10 s after their last request: only the forks and
  # the process totals count them, not the per-thread t0/t1 snapshots.
  TGID0=$(cat $TR/options/record-tgid); BUF0=$(cut -d" " -f1 $TR/buffer_size_kb)
  echo 1 > $TR/options/record-tgid
  echo 4096 > $TR/buffer_size_kb
  echo 1 > $TR/events/sched/sched_process_fork/enable
  echo 1 > $TR/events/sched/sched_process_exec/enable
  echo 1 > $TR/tracing_on
  TRACE=1
fi
echo "trace=$TRACE dir=$TR" > $OUT/trace_info
snap t0
sleep $SECS
snap t1
if [ $TRACE = 1 ]; then
  echo 0 > $TR/tracing_on
  cat $TR/trace > $OUT/trace
  echo 0 > $TR/events/sched/sched_process_fork/enable
  echo 0 > $TR/events/sched/sched_process_exec/enable
  echo $TGID0 > $TR/options/record-tgid
  echo $BUF0 > $TR/buffer_size_kb
  echo > $TR/trace
fi
echo DONE > $OUT/done
