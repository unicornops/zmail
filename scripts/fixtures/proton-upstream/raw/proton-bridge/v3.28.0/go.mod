module github.com/ProtonMail/proton-bridge/v3

go 1.26.1

require (
	github.com/ProtonMail/gluon v0.17.1-0.20260901104218-7e800978ab4a
	github.com/ProtonMail/go-proton-api v0.4.1-0.20261012093000-444444444444
	github.com/ProtonMail/gopenpgp/v2 v2.10.0-proton
	github.com/emersion/go-message v0.18.1
)

require (
	github.com/ProtonMail/go-srp v0.0.7 // indirect
)

replace (
	github.com/emersion/go-message => github.com/ProtonMail/go-message v0.13.1-0.20240919135104-666666666666
	github.com/go-resty/resty/v2 => github.com/ProtonMail/resty/v2 v2.0.0-20250929142426-e3dc6308c80b
)
