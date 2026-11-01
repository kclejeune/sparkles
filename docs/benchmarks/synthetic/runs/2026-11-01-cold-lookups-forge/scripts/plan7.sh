#!/usr/bin/env bash
cd ~/lanes/cold-lookups-4
until [ -f plan6.done ]; do sleep 30; done
./insn.sh insn-att1 2 base fin att1
echo done > plan7.done
