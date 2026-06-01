#!/usr/bin/env bash

set -euo pipefail

script_dir="$(readlink -f "$(dirname "$(readlink -f "$0")")")"

duration="${1:-120}"

cd "$script_dir/.."

out_dir="${2:-benchmark_data}"

if [[ -z $out_dir ]]; then
	printf "no out dir\n" >&2
	exit 1
fi

mkdir -p "$out_dir"

cargo build --package examples --all-targets --release

pid_file=examples/prefix/logs/nginx.pid

stop_nginx() {
	if [[ -f $pid_file ]]; then
		kill -0 "$(cat $pid_file)" 2>/dev/null || rm -rf "$pid_file"
	fi
	local deadline=$((SECONDS + 10))
	while [[ -f $pid_file ]] && ((SECONDS < deadline)); do
		kill -TERM "$(cat $pid_file)"
		wait
		[[ -f $pid_file ]] && sleep 1
	done
	! [[ -f $pid_file ]]
}

# Clear any stale process
stop_nginx

temp=$(mktemp -d)

trap 'stop_nginx; [[ -d $temp ]] && rm -rf "$temp"' EXIT INT TERM

start_nginx() {
	local batch_size="$1"
	shift
	local heaptrack="$1"
	shift

	if [[ -n $heaptrack ]]; then
		TICKLE_BATCH_SIZE="$batch_size" heaptrack -q --record-only -o "$heaptrack" examples/prefix/sbin/nginx -c conf/benchmark.conf &
	else
		TICKLE_BATCH_SIZE="$batch_size" examples/prefix/sbin/nginx -c conf/benchmark.conf &
	fi

	deadline=$((SECONDS + 10))
	ready=0
	while ((SECONDS < deadline)); do
		if [[ -s $pid_file ]] &&
			(exec 3<>/dev/tcp/127.0.0.1/9000) 2>/dev/null; then
			exec 3<&- 3>&-
			ready=1
			break
		fi
		sleep 0.2
	done
	if ((!ready)); then
		printf "nginx not ready on :9000 after 10s\n" >&2
		exit 1
	fi
}

wrk() {
	# shellcheck disable=SC2030,SC2031
	(
		# empty string disable reporting, i.e. for the heaptrack run
		export WRK_REPORT="$1"
		shift
		export WRK_EXTRA_FIELDS="$1"
		shift
		url="$1"
		shift

		command wrk \
			-t3 -c100 -d"${duration}s" --latency \
			${WRK_REPORT:+ -s "$script_dir/report.lua"} \
			"$url" \
			"$@"
	)
}

wrk2() {
	# shellcheck disable=SC2030,SC2031
	(
		export WRK_REPORT="$1"
		shift
		export WRK_EXTRA_FIELDS="$1"
		shift
		r="$1"
		shift
		url="$1"
		shift

		command wrk2 \
			-t3 -c100 -d"${duration}s" --latency \
			-R"$r" \
			-s "$script_dir/report.lua" "$url" \
			"$@"
	)
}

# extract min rps of passed csv files and mult with .7 to get -R for wrk2 run
pace() {
	xan cat rows "$@" |
		xan agg 'min(rps) as m' |
		xan map 'floor(m * 0.7) as r' |
		xan select r |
		xan behead
}

run_group() {
	prefix=$1
	shift
	group=$1
	shift

	printf "\n\n### Group %s ###\n\n" "$prefix"

	while read -r name uri batch_size batch_size_display; do
		if [[ $name != "$prefix"_* ]]; then
			printf "name must start with %s_, got %s\n" "$prefix" "$name" >&2
			exit 1
		fi
		for rep in $(seq 3); do
			start_nginx "$batch_size" ""
			printf "\n### wrk %s,%d ###\n\n" "$name""${batch_size_display:+" (batch_size=$batch_size_display)"}" "$rep"
			report="$temp/$name.wrk${batch_size_display:+".$batch_size_display"}.$rep.csv"
			extra="name=$name,mode=wrk,batch_size=$batch_size_display,rep=$rep,r="
			wrk "$report" "$extra" "$uri"
			stop_nginx
		done
	done <<<"$group"

	r=$(pace "$temp/$prefix"_*.csv)

	printf "\n\n### Pace for %s group: %d ###\n\n" "$prefix" "$r"

	while read -r name uri batch_size batch_size_display; do
		for rep in $(seq 3); do
			start_nginx "$batch_size" ""
			printf "\n### wrk2 %s,%d ###\n\n" "$name""${batch_size_display:+" (batch_size=$batch_size_display)"}" "$rep"
			report="$temp/$name.wrk2${batch_size_display:+".$batch_size_display"}.$rep.csv"
			extra="name=$name,mode=wrk2,batch_size=$batch_size_display,rep=$rep,r=$r"
			wrk2 "$report" "$extra" "$r" "$uri"
			stop_nginx
		done
	done <<<"$group"

	heaptrack_out="$temp/heaptrack/heaptrack"

	while read -r name uri batch_size batch_size_display; do
		start_nginx "$batch_size" "$heaptrack_out"
		printf "\n### heaptrack %s ###\n\n" "$name""${batch_size_display:+" (batch_size=$batch_size_display)"}"
		wrk "" "" "$uri"
		stop_nginx
		if ! [[ -f $heaptrack_out.zst ]]; then
			printf "No heaptrack file at %s\n" "$heaptrack_out.zst"
			exit 1
		fi

		heaptrack_print "$heaptrack_out.zst" | awk -F': ' \
			-v name="$name" -v batch_size="$batch_size_display" \
			'/peak heap memory consumption/ {printf "%s,%s,%s\n", name, batch_size, $2}' >>"$temp/heaptrack/heaptrack.csv"
	done <<<"$group"
}

mkdir -p "$temp/heaptrack"
printf "name,batch_size,peak_heap\n" >"$temp/heaptrack/heaptrack.csv"

resolve_group=$(
	# name url TICKLE_BATCH_SIZE [batch_size in csv]
	# leave 4. arg empty for ngx runs; it's not affected and we want an empty cell in csv
	cat <<-'EOF'
		resolve_ngx http://127.0.0.1:9000/benchmark/resolve/ngx 1
		resolve_sync http://127.0.0.1:9000/benchmark/resolve/sync 1
		resolve_tickle http://127.0.0.1:9000/benchmark/resolve/tickle 1 1
		resolve_tickle http://127.0.0.1:9000/benchmark/resolve/tickle 8 8
		resolve_tickle http://127.0.0.1:9000/benchmark/resolve/tickle 1024 1024
	EOF
)

run_group "resolve" "$resolve_group"

hyper_group=$(
	cat <<-'EOF'
		hyper_ngx http://127.0.0.1:9000/benchmark/hyper/ngx 1
		hyper_tickle http://127.0.0.1:9000/benchmark/hyper/tickle 1 1
		hyper_tickle http://127.0.0.1:9000/benchmark/hyper/tickle 8 8
		hyper_tickle http://127.0.0.1:9000/benchmark/hyper/tickle 1024 1024
		hyper_tokio http://127.0.0.1:9000/benchmark/hyper/tokio 1 1
		hyper_tokio http://127.0.0.1:9000/benchmark/hyper/tokio 8 8
		hyper_tokio http://127.0.0.1:9000/benchmark/hyper/tokio 1024 1024
	EOF
)

run_group "hyper" "$hyper_group"

stamp="$(date -Iseconds)"

out_file="$out_dir"/"$stamp".csv
xan cat rows "$temp"/*.csv | xan sort -s name,mode,batch_size,rep -o "$out_file"
xan view "$out_file"

xan sort -s name,batch_size "$temp/heaptrack/heaptrack.csv" -o "$out_dir"/"$stamp"_heaptrack.csv
xan view "$out_dir"/"$stamp"_heaptrack.csv
