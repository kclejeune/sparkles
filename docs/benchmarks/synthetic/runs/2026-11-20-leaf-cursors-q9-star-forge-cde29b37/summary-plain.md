# q9-star rerun at 12 threads, 6 rounds, engine sparkles-plain

Builds: base bccc5f7b, main 5f3f69c2, tip cde29b37

Answer fingerprints differing from the first arm: none

## streaming t12

- base bccc5f7b: median 26.50 ms; arm medians 29.4 26.6 27.1 26.6 26.9 24.6 26.0 27.7 24.1 25.5 26.5 25.9; fast requests 41/300, fast arms 0/12; same-binary max spread 10.3%
- main 5f3f69c2: median 27.29 ms; arm medians 26.2 28.0 26.9 27.9 25.9 28.2 27.2 27.1 26.9 27.2 28.2 26.8; fast requests 15/300, fast arms 0/12; same-binary max spread 8.6%
- tip cde29b37: median 26.49 ms; arm medians 27.6 27.8 26.5 27.7 27.8 23.7 26.7 25.2 25.6 26.4 26.8 25.6; fast requests 45/300, fast arms 0/12; same-binary max spread 17.2%

- tip/main: pooled 0.971 (95% over processes 0.942 to 0.998); pairs 1.051 0.995 0.985 0.992 1.071 0.842 0.981 0.928 0.951 0.968 0.949 0.957; min 0.842, max 1.071; above 1 in every pair: False; below 1 in every pair: False
- main/base: pooled 1.030 (95% over processes 1.004 to 1.065); pairs 0.893 1.050 0.992 1.048 0.963 1.143 1.047 0.979 1.117 1.069 1.063 1.036; min 0.893, max 1.143; above 1 in every pair: False; below 1 in every pair: False
- tip/base: pooled 1.000 (95% over processes 0.966 to 1.039); pairs 0.938 1.045 0.977 1.039 1.032 0.962 1.027 0.909 1.062 1.035 1.009 0.992; min 0.909, max 1.062; above 1 in every pair: False; below 1 in every pair: False

## eager t12

- base bccc5f7b: median 26.11 ms; arm medians 25.7 25.7 27.3 25.9 21.6 26.9 26.3 25.6 26.7 26.1 27.4 26.8; fast requests 51/300, fast arms 1/12; same-binary max spread 24.9%
- main 5f3f69c2: median 26.30 ms; arm medians 27.2 26.9 26.3 27.8 14.3 25.2 25.9 26.3 23.4 26.9 26.9 26.6; fast requests 49/300, fast arms 1/12; same-binary max spread 75.9%
- tip cde29b37: median 25.83 ms; arm medians 25.9 25.6 27.8 27.4 23.6 25.5 26.5 26.6 23.8 26.6 26.0 25.6; fast requests 44/300, fast arms 0/12; same-binary max spread 11.8%

- tip/main: pooled 0.982 (95% over processes 0.948 to 1.031); pairs 0.953 0.954 1.055 0.987 1.645 1.011 1.023 1.011 1.015 0.988 0.968 0.964; min 0.953, max 1.645; above 1 in every pair: False; below 1 in every pair: False
- main/base: pooled 1.007 (95% over processes 0.961 to 1.042); pairs 1.058 1.046 0.965 1.073 0.664 0.936 0.986 1.029 0.878 1.034 0.983 0.993; min 0.664, max 1.073; above 1 in every pair: False; below 1 in every pair: False
- tip/base: pooled 0.989 (95% over processes 0.951 to 1.032); pairs 1.008 0.998 1.017 1.058 1.093 0.946 1.010 1.040 0.891 1.021 0.951 0.957; min 0.891, max 1.093; above 1 in every pair: False; below 1 in every pair: False

