#!/usr/bin/env bash
set -euo pipefail

bytes_of() {
  wc -c <"$1"
}

usage() {
  cat <<EOF
Usage: $0 [--certs-dir <path>]
Default output directory: tests/integration/certs (from current directory)
EOF
}

issue_device_cert() {
  local certs_dir="$1"
  local name="$2"
  local cn="$3"
  local san="$4"
  local key_pem="$certs_dir/$name.key"
  local csr="$certs_dir/$name.csr"
  local crt="$certs_dir/$name.crt"
  local der="$certs_dir/$name.der"
  local sec1_der="$certs_dir/$name.key.der"
  local pkcs8="$certs_dir/$name.key.pkcs8.der"
  local ext="$certs_dir/$name.ext"

  printf '%s\n' "--> $name: key + cert"
  openssl ecparam -name prime256v1 -genkey -noout -out "$key_pem"
  openssl req -new -key "$key_pem" -out "$csr" -subj "/O=Swarm Test/CN=$cn"
  cat >"$ext" <<EOF
basicConstraints=CA:FALSE
keyUsage=critical,digitalSignature
extendedKeyUsage=clientAuth,serverAuth
subjectAltName=$san
EOF
  openssl x509 -req -in "$csr" -CA "$certs_dir/ca.crt" -CAkey "$certs_dir/ca.key" -CAcreateserial -out "$crt" -days 3650 -extfile "$ext"
  openssl x509 -in "$crt" -outform DER -out "$der"
  openssl ec -in "$key_pem" -outform DER -out "$sec1_der"
  openssl pkcs8 -topk8 -nocrypt -in "$key_pem" -outform DER -out "$pkcs8"
  rm -f "$csr" "$ext"

  printf '    %s.der %sB / %s.key.der %sB / %s.key.pkcs8.der %sB\n' \
    "$name" "$(bytes_of "$der")" "$name" "$(bytes_of "$sec1_der")" "$name" "$(bytes_of "$pkcs8")"
}

certs_dir=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      usage
      exit 0
      ;;
    --certs-dir)
      [[ $# -ge 2 ]] || { echo "missing value for --certs-dir" >&2; exit 1; }
      certs_dir="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$certs_dir" ]]; then
  certs_dir="$(pwd -P)/tests/integration/certs"
else
  certs_dir="$(readlink -f "$certs_dir")"
fi

mkdir -p "$certs_dir"
printf '==> Generating test PKI in %s\n' "$certs_dir"

ca_key="$certs_dir/ca.key"
ca_crt="$certs_dir/ca.crt"
ca_der="$certs_dir/ca.der"

printf '%s\n' "--> CA key + self-signed cert"
openssl ecparam -name prime256v1 -genkey -noout -out "$ca_key"
openssl req -new -x509 -key "$ca_key" -out "$ca_crt" -days 3650 \
  -subj "/O=Swarm Test/CN=swarm-test-ca" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"
openssl x509 -in "$ca_crt" -outform DER -out "$ca_der"
printf '    ca.crt %sB PEM / ca.der %sB DER\n' "$(bytes_of "$ca_crt")" "$(bytes_of "$ca_der")"

issue_device_cert "$certs_dir" "laptop" "laptop-test" "DNS:laptop-test"
issue_device_cert "$certs_dir" "esp" "esp32c6-test" "DNS:esp32c6-test"

printf '%s\n' "--> wrong-ca (different root for negative tests)"
wca_key="$certs_dir/wrong-ca.key"
wca_crt="$certs_dir/wrong-ca.crt"
wca_der="$certs_dir/wrong-ca.der"
openssl ecparam -name prime256v1 -genkey -noout -out "$wca_key"
openssl req -new -x509 -key "$wca_key" -out "$wca_crt" -days 3650 \
  -subj "/O=Swarm Test/CN=wrong-ca" \
  -addext "basicConstraints=critical,CA:TRUE"
openssl x509 -in "$wca_crt" -outform DER -out "$wca_der"
rm -f "$wca_key" "$wca_crt" "$certs_dir/ca.srl"

printf '\n==> Done.  Certificate summary:\n\n'
printf '  ca.der               %sB  -- Swarm test CA\n' "$(bytes_of "$ca_der")"
printf '  laptop.der           %sB  -- Laptop cert for rustls server\n' "$(bytes_of "$certs_dir/laptop.der")"
printf '  laptop.key.der       %sB  -- Laptop key SEC1 DER for embedded-tls\n' "$(bytes_of "$certs_dir/laptop.key.der")"
printf '  laptop.key.pkcs8.der %sB  -- Laptop key PKCS8 DER for rustls\n' "$(bytes_of "$certs_dir/laptop.key.pkcs8.der")"
printf '  esp.der              %sB  -- ESP32 cert for embedded-tls client\n' "$(bytes_of "$certs_dir/esp.der")"
printf '  esp.key.der          %sB  -- ESP32 key SEC1 DER for embedded-tls\n' "$(bytes_of "$certs_dir/esp.key.der")"
printf '  esp.key.pkcs8.der    %sB  -- ESP32 key PKCS8 DER\n' "$(bytes_of "$certs_dir/esp.key.pkcs8.der")"
printf '  wrong-ca.der         %sB  -- Wrong CA for negative tests\n\n' "$(bytes_of "$wca_der")"

printf '%s\n' "  embedded-tls: Certificate::X509(X.der), priv_key = X.key.der"
printf '%s\n' "  rustls:       CertificateDer::from(X.der), PrivateKeyDer::from(X.key.pkcs8.der)"
