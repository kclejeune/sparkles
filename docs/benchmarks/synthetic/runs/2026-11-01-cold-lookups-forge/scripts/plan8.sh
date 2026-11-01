#!/usr/bin/env bash
cd ~/lanes/cold-lookups-4
until [ -f plan7.done ]; do sleep 30; done
A="base|||warm|" B="att1|||warm|" ./ab.sh att1-warmflag 10m 21 4 exists-join expr-bind-group optional-count group-avg expr-agg-arg
echo done > plan8.done
