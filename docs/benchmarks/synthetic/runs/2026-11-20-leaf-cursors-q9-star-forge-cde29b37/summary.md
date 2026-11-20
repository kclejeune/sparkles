# q9-star rerun at 12 threads, 6 rounds, engine sparkles

Builds: base bccc5f7b, main 5f3f69c2, tip cde29b37

Answer fingerprints differing from the first arm: none

## streaming t12

- base bccc5f7b: median 25.54 ms; arm medians 26.5 25.6 22.9 27.4 25.9 22.8 20.4 28.7 24.4 24.9 28.3 27.6; fast requests 70/300, fast arms 1/12; same-binary max spread 40.7%
- main 5f3f69c2: median 27.53 ms; arm medians 29.5 27.5 26.6 27.6 28.0 27.3 27.8 26.1 25.9 27.9 27.8 27.4; fast requests 22/300, fast arms 0/12; same-binary max spread 7.7%
- tip cde29b37: median 26.60 ms; arm medians 26.8 27.9 26.5 24.9 24.1 27.5 28.1 27.0 26.9 25.9 27.3 23.0; fast requests 61/300, fast arms 0/12; same-binary max spread 19.0%

- tip/main: pooled 0.966 (95% over processes 0.944 to 0.991); pairs 0.908 1.013 0.999 0.900 0.864 1.007 1.010 1.033 1.040 0.927 0.982 0.838; min 0.838, max 1.040; above 1 in every pair: False; below 1 in every pair: False
- main/base: pooled 1.078 (95% over processes 1.011 to 1.134); pairs 1.114 1.078 1.162 1.008 1.078 1.194 1.362 0.909 1.060 1.120 0.982 0.996; min 0.909, max 1.362; above 1 in every pair: False; below 1 in every pair: False
- tip/base: pooled 1.042 (95% over processes 0.976 to 1.099); pairs 1.011 1.092 1.161 0.908 0.931 1.203 1.376 0.940 1.103 1.038 0.965 0.834; min 0.834, max 1.376; above 1 in every pair: False; below 1 in every pair: False

## eager t12

- base bccc5f7b: median 26.73 ms; arm medians 27.0 27.7 28.1 25.5 25.2 26.3 27.5 26.3 27.8 25.6 25.5 26.7; fast requests 13/300, fast arms 0/12; same-binary max spread 10.2%
- main 5f3f69c2: median 26.48 ms; arm medians 27.2 27.2 27.0 26.0 24.2 13.2 25.9 26.0 26.3 27.1 27.5 27.3; fast requests 53/300, fast arms 1/12; same-binary max spread 83.9%
- tip cde29b37: median 26.64 ms; arm medians 27.2 26.1 27.0 27.5 25.3 27.0 26.0 23.8 27.0 26.3 27.0 26.9; fast requests 52/300, fast arms 0/12; same-binary max spread 9.4%

- tip/main: pooled 1.006 (95% over processes 0.975 to 1.044); pairs 1.002 0.957 0.999 1.057 1.044 2.052 1.005 0.916 1.026 0.968 0.982 0.984; min 0.916, max 2.052; above 1 in every pair: False; below 1 in every pair: False
- main/base: pooled 0.991 (95% over processes 0.951 to 1.024); pairs 1.006 0.982 0.961 1.019 0.959 0.500 0.942 0.987 0.945 1.059 1.076 1.022; min 0.500, max 1.076; above 1 in every pair: False; below 1 in every pair: False
- tip/base: pooled 0.997 (95% over processes 0.966 to 1.026); pairs 1.008 0.941 0.960 1.077 1.001 1.025 0.947 0.904 0.970 1.026 1.057 1.006; min 0.904, max 1.077; above 1 in every pair: False; below 1 in every pair: False

