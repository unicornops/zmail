// zmail proton-harness: imports .eml fixtures into the harness account.
//
// The fake server's gRPC control service creates users, addresses and
// labels but no messages, so this tool signs in as the seeded user through
// go-proton-api's client, unlocks the address keys and imports each
// message the way Bridge imports IMAP APPENDs (ImportMessages), then stars
// the ones the manifest asks for. It is built into the harness image next
// to the fake server (see Dockerfile) and run by scripts/proton-harness.sh.
// Test-only; under go-proton-api's MIT licence like the patches.
//
// Usage: zmail-seed --api URL --ca FILE --username NAME --password PASS --manifest FILE
//
// The manifest is a JSON list of {"file", "address", "labels", "unread",
// "starred"}: "file" is relative to the manifest, "address" is an email of
// the account, and "labels" are label or folder names ("Inbox", "Work").
// The tool prints a JSON list of {"file", "id", "subject"} on stdout.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"github.com/ProtonMail/gluon/async"
	"github.com/ProtonMail/go-proton-api"
	"github.com/bradenaw/juniper/stream"
)

type entry struct {
	File    string   `json:"file"`
	Address string   `json:"address"`
	Labels  []string `json:"labels"`
	Unread  bool     `json:"unread"`
	Starred bool     `json:"starred"`
	Subject string   `json:"subject"`
}

type seeded struct {
	File    string `json:"file"`
	ID      string `json:"id"`
	Subject string `json:"subject"`
}

func main() {
	api := flag.String("api", "", "API base URL")
	caFile := flag.String("ca", "", "CA certificate (PEM) to trust")
	username := flag.String("username", "", "account name")
	password := flag.String("password", "", "account password")
	manifestFile := flag.String("manifest", "", "manifest JSON file")
	flag.Parse()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	if err := run(ctx, *api, *caFile, *username, *password, *manifestFile); err != nil {
		log.Fatalf("zmail-seed: %v", err)
	}
}

func run(ctx context.Context, api, caFile, username, password, manifestFile string) error {
	caPEM, err := os.ReadFile(caFile)
	if err != nil {
		return err
	}

	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(caPEM) {
		return fmt.Errorf("no certificates in %s", caFile)
	}

	manager := proton.New(
		proton.WithHostURL(api),
		proton.WithAppVersion("zmail@0.0.0-harness"),
		proton.WithTransport(&http.Transport{TLSClientConfig: &tls.Config{RootCAs: roots, MinVersion: tls.VersionTLS12}}),
	)
	defer manager.Close()

	client, _, err := manager.NewClientWithLogin(ctx, username, []byte(password))
	if err != nil {
		return fmt.Errorf("login: %w", err)
	}
	defer client.Close()

	user, err := client.GetUser(ctx)
	if err != nil {
		return err
	}

	addresses, err := client.GetAddresses(ctx)
	if err != nil {
		return err
	}

	salts, err := client.GetSalts(ctx)
	if err != nil {
		return err
	}

	keyPass, err := salts.SaltForKey([]byte(password), user.Keys.Primary().ID)
	if err != nil {
		return err
	}

	_, addrKRs, err := proton.Unlock(user, addresses, keyPass, async.NoopPanicHandler{})
	if err != nil {
		return err
	}

	labels, err := client.GetLabels(ctx, proton.LabelTypeSystem, proton.LabelTypeFolder, proton.LabelTypeLabel)
	if err != nil {
		return err
	}

	labelIDs := make(map[string]string, len(labels))
	for _, label := range labels {
		labelIDs[label.Name] = label.ID
	}

	addressIDs := make(map[string]string, len(addresses))
	for _, address := range addresses {
		addressIDs[address.Email] = address.ID
	}

	raw, err := os.ReadFile(manifestFile)
	if err != nil {
		return err
	}

	var entries []entry
	if err := json.Unmarshal(raw, &entries); err != nil {
		return fmt.Errorf("manifest: %w", err)
	}

	var out []seeded

	for _, e := range entries {
		addrID, ok := addressIDs[e.Address]
		if !ok {
			return fmt.Errorf("%s: unknown address %q", e.File, e.Address)
		}

		var ids []string
		for _, name := range e.Labels {
			id, ok := labelIDs[name]
			if !ok {
				return fmt.Errorf("%s: unknown label %q", e.File, name)
			}

			ids = append(ids, id)
		}

		literal, err := os.ReadFile(filepath.Join(filepath.Dir(manifestFile), e.File))
		if err != nil {
			return err
		}

		res, err := client.ImportMessages(ctx, addrKRs[addrID], 1, 1, proton.ImportReq{
			Metadata: proton.ImportMetadata{
				AddressID: addrID,
				LabelIDs:  ids,
				Unread:    proton.Bool(e.Unread),
				Flags:     proton.MessageFlagReceived,
			},
			Message: literal,
		})
		if err != nil {
			return fmt.Errorf("%s: %w", e.File, err)
		}

		results, err := stream.Collect(ctx, stream.Stream[proton.ImportRes](res))
		if err != nil {
			return fmt.Errorf("%s: %w", e.File, err)
		}

		if len(results) != 1 {
			return fmt.Errorf("%s: %d import results", e.File, len(results))
		}

		if e.Starred {
			if err := client.LabelMessages(ctx, []string{results[0].MessageID}, proton.StarredLabel); err != nil {
				return fmt.Errorf("%s: star: %w", e.File, err)
			}
		}

		out = append(out, seeded{File: e.File, ID: results[0].MessageID, Subject: e.Subject})
	}

	return json.NewEncoder(os.Stdout).Encode(out)
}
