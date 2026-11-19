# Changelog

## [0.5.0](https://github.com/scottmckendry/flakemill/compare/v0.4.2...v0.5.0) (2026-11-19)


### Features

* **dashboard:** add storage section for pvcs ([41df299](https://github.com/scottmckendry/flakemill/commit/41df2998f17b2574831f77e9ac290ffdb77bf3c2))
* local cache serving with nix-serve-ng ([54b6787](https://github.com/scottmckendry/flakemill/commit/54b678763ba811f327622b985e8d03517e3fc220))


### Bug Fixes

* **dashboard:** snow drawing over cards ([00fa53a](https://github.com/scottmckendry/flakemill/commit/00fa53a7b53c8536a30e07e86c03145db9ff592c))

## [0.4.2](https://github.com/scottmckendry/flakemill/compare/v0.4.1...v0.4.2) (2026-11-18)


### Bug Fixes

* **client:** compare locks semantically ([0a5b74f](https://github.com/scottmckendry/flakemill/commit/0a5b74fc8f8cf8b9c74c2c5c719fbd71596f5141))
* **container:** set user before cachix ([76172c3](https://github.com/scottmckendry/flakemill/commit/76172c3a1d3c5d0e14ef7ed23a2d521b7a9af24e))
* **job:** add active build guard ([93c846e](https://github.com/scottmckendry/flakemill/commit/93c846efd830641ba36522fe05a6bd2773483b13))
* **nix:** add gc to the end of each build ([a118635](https://github.com/scottmckendry/flakemill/commit/a118635fa24f4224bfc273e8606876052737246f))
* **nix:** deprecated flake arg ([65c4aa4](https://github.com/scottmckendry/flakemill/commit/65c4aa414db7a61c0e03736390fb592c76226bb1))

## [0.4.1](https://github.com/scottmckendry/flakemill/compare/v0.4.0...v0.4.1) (2026-11-17)


### Bug Fixes

* **dashboard:** clean up html ([aa1381e](https://github.com/scottmckendry/flakemill/commit/aa1381ef968a7763d405160a4e8a78ca3acbdf08))
* **dashboard:** mobile ux tweaks ([75f530a](https://github.com/scottmckendry/flakemill/commit/75f530a79a63c9bdc8959dd6adaddbf47fc99370))

## [0.4.0](https://github.com/scottmckendry/flakemill/compare/v0.3.0...v0.4.0) (2026-11-16)


### Features

* **client:** show changed inputs on apply/check ([99bece6](https://github.com/scottmckendry/flakemill/commit/99bece649949962b7c5c008c006dd716bf980a6e))
* **dashboard:** design update ([287727a](https://github.com/scottmckendry/flakemill/commit/287727a4ca542fed1c41e7d191df2a5bf8b43f11))


### Bug Fixes

* **container:** validate nix install and seed cachix config ([172ef6d](https://github.com/scottmckendry/flakemill/commit/172ef6daed28741bb036b46ad27991d1d596dff4))
* **dashboard:** add favicon ([400c2b0](https://github.com/scottmckendry/flakemill/commit/400c2b0783814d6e948a0d6df1c3c023a8996a3c))
* **flake:** include sha in package version ([05c8101](https://github.com/scottmckendry/flakemill/commit/05c810141e071b5bcebc5911a8696c0b94b25160))

## [0.3.0](https://github.com/scottmckendry/flakemill/compare/v0.2.1...v0.3.0) (2026-11-15)


### Features

* add flakemill client ([ee7c6d6](https://github.com/scottmckendry/flakemill/commit/ee7c6d6d3e26bf8fca8a7cdc5b4c7e391092367e))
* show version and source link in dashboard ([a0f91cf](https://github.com/scottmckendry/flakemill/commit/a0f91cf169e82638c3e892a8df3a20380711408d))


### Bug Fixes

* **container:** cache cargo outputs ([58bb835](https://github.com/scottmckendry/flakemill/commit/58bb8355178ec006243a89cdabcf6f41e6802c81))
* **container:** disable nix sandbox ([68d5e0e](https://github.com/scottmckendry/flakemill/commit/68d5e0edc9136495e636a844ccf2eb7d0a284e33))
* **dashboard:** improve usability ([f53a901](https://github.com/scottmckendry/flakemill/commit/f53a9016b8658169551f7c829c362e9147b3bd0f))

## [0.2.1](https://github.com/scottmckendry/flakemill/compare/v0.2.0...v0.2.1) (2026-11-15)


### Bug Fixes

* misc fixes, clean up and tests ([5546605](https://github.com/scottmckendry/flakemill/commit/5546605298194bcd1c217f93bc22369abb07ab62))

## [0.2.0](https://github.com/scottmckendry/flakemill/compare/v0.1.1...v0.2.0) (2026-11-15)


### Features

* add basic dashboard ([9a5289e](https://github.com/scottmckendry/flakemill/commit/9a5289e3714591d26995820e12077657e320baf2))


### Bug Fixes

* lock comparison post update ([3f09cfd](https://github.com/scottmckendry/flakemill/commit/3f09cfd5b3523b61be9bea60537c9c11754ad639))

## [0.1.1](https://github.com/scottmckendry/flakemill/compare/v0.1.0...v0.1.1) (2026-11-14)


### Bug Fixes

* **config:** remove undocumented defaults ([b23617b](https://github.com/scottmckendry/flakemill/commit/b23617b98b49a61038bb4dc6a970e8dd7883abfc))
* **container:** seed nix store ([5c66bfc](https://github.com/scottmckendry/flakemill/commit/5c66bfc803418091d7541f147e3b12be8390d270))

## [0.1.0](https://github.com/scottmckendry/flakemill/compare/v0.1.0...v0.1.0) (2026-11-14)


### Features

* containerise with podman ([cd1bc77](https://github.com/scottmckendry/flakemill/commit/cd1bc77b2d86391d15caff20de20522f5ea8cbee))
* initial commit ([15f7bc6](https://github.com/scottmckendry/flakemill/commit/15f7bc6741eaf44c6a8255bfa9cd19daa2adc7a5))


### Miscellaneous Chores

* fix formatting ([905ed4a](https://github.com/scottmckendry/flakemill/commit/905ed4a9f5aae2afe82b1770dfdc553e19387528))
