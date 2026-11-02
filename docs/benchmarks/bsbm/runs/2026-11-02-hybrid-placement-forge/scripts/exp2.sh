#!/usr/bin/env bash
cd ~/lanes/thread-placement-449
QUERIES="bi-q3 bi-q5 bi-q8" EXPLAIN="bi-q3-2 bi-q3-3 bi-q5-3" EXPLAIN_N=3 ./run.sh exp2 4 all20 all12 p12 pe19 p12x20 e7 all16
