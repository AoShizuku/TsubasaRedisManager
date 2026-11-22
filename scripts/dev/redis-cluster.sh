#!/bin/sh
# 在**单个容器内**跑 6 个 redis 节点组成集群（3 主 3 从）。
#
# 为什么挤在一个容器里而不是 6 个：集群客户端会跟随节点自己公告的地址。
# 一节点一容器时，节点之间要用 docker 内网 IP 互相通信，而宿主机（尤其是
# Docker Desktop）连不到那些 IP；反过来公告 127.0.0.1 的话，节点从自己的
# 容器里连 127.0.0.1 又只会连到它自己。放进同一个网络命名空间，
# 127.0.0.1:70xx 对节点之间和对宿主机就是同一个含义，两边都成立。
set -e

PORTS="7000 7001 7002 7003 7004 7005"

for p in $PORTS; do
  redis-server --port "$p" --bind 0.0.0.0 \
    --cluster-enabled yes --cluster-config-file "/tmp/nodes-$p.conf" \
    --cluster-node-timeout 5000 \
    --cluster-announce-ip 127.0.0.1 --cluster-announce-port "$p" \
    --save '' --appendonly no --daemonize yes
done

# 等所有节点起来再组建，否则 cluster create 会打在半死的节点上
for p in $PORTS; do
  until redis-cli -p "$p" ping >/dev/null 2>&1; do sleep 0.2; done
done

NODES=""
for p in $PORTS; do NODES="$NODES 127.0.0.1:$p"; done
# shellcheck disable=SC2086
redis-cli --cluster create $NODES --cluster-replicas 1 --cluster-yes

# 容器要一直活着，pid 1 不能退
exec tail -f /dev/null
