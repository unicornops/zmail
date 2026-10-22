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
//
// With --contacts FILE it also creates contacts: a JSON list of {"email",
// "name", "sign", "encrypt", "scheme", "mime_type", "key"}. Each becomes a
// contact whose signed card (signed by the user keys, as Proton's clients
// sign them) carries the X-PM-* send settings for the address and, with
// "key", a freshly generated public key. The fake server checks card
// signatures against the address keys when a contact is created, so the
// contact is created with a clear card and then given its signed card.
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/base64"
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
	"github.com/ProtonMail/gopenpgp/v2/crypto"
	"github.com/bradenaw/juniper/stream"
	"github.com/emersion/go-vcard"
)

type entry struct {
	File    string   `json:"file"`
	Address string   `json:"address"`
	Labels  []string `json:"labels"`
	Unread  bool     `json:"unread"`
	Starred bool     `json:"starred"`
	Subject string   `json:"subject"`
}

type contact struct {
	Email    string `json:"email"`
	Name     string `json:"name"`
	Sign     string `json:"sign"`
	Encrypt  string `json:"encrypt"`
	Scheme   string `json:"scheme"`
	MIMEType string `json:"mime_type"`
	Key      bool   `json:"key"`
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
	contactsFile := flag.String("contacts", "", "contacts JSON file (optional)")
	flag.Parse()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	if err := run(ctx, *api, *caFile, *username, *password, *manifestFile, *contactsFile); err != nil {
		log.Fatalf("zmail-seed: %v", err)
	}
}

func run(ctx context.Context, api, caFile, username, password, manifestFile, contactsFile string) error {
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
		proton.WithAppVersion("linux-bridge@3.27.1"),
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

	userKR, addrKRs, err := proton.Unlock(user, addresses, keyPass, async.NoopPanicHandler{})
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

	if contactsFile != "" {
		if err := seedContacts(ctx, client, userKR, contactsFile); err != nil {
			return err
		}
	}

	return json.NewEncoder(os.Stdout).Encode(out)
}

func seedContacts(ctx context.Context, client *proton.Client, userKR *crypto.KeyRing, contactsFile string) error {
	raw, err := os.ReadFile(contactsFile)
	if err != nil {
		return err
	}

	var contacts []contact
	if err := json.Unmarshal(raw, &contacts); err != nil {
		return fmt.Errorf("contacts: %w", err)
	}

	for _, c := range contacts {
		clearCard, err := proton.NewCard(userKR, proton.CardTypeClear)
		if err != nil {
			return err
		}

		signed, err := proton.NewCard(userKR, proton.CardTypeSigned)
		if err != nil {
			return err
		}

		fields := []struct{ key, value string }{
			{vcard.FieldFormattedName, c.Name},
			{vcard.FieldEmail, c.Email},
			{proton.FieldPMSign, c.Sign},
			{proton.FieldPMEncrypt, c.Encrypt},
			{proton.FieldPMScheme, c.Scheme},
			{proton.FieldPMMIMEType, c.MIMEType},
		}

		if c.Key {
			key, err := crypto.GenerateKey(c.Name, c.Email, "x25519", 0)
			if err != nil {
				return err
			}

			pub, err := key.GetPublicKey()
			if err != nil {
				return err
			}

			fields = append(fields, struct{ key, value string }{vcard.FieldKey, "data:application/pgp-keys;base64," + base64.StdEncoding.EncodeToString(pub)})
		}

		for _, f := range fields {
			if f.value == "" {
				continue
			}

			group := "item1"
			if f.key == vcard.FieldFormattedName {
				group = ""
			}

			if f.key == vcard.FieldFormattedName || f.key == vcard.FieldEmail {
				if err := clearCard.Add(userKR, f.key, &vcard.Field{Value: f.value, Group: group}); err != nil {
					return err
				}
			}

			if err := signed.Add(userKR, f.key, &vcard.Field{Value: f.value, Group: group}); err != nil {
				return err
			}
		}

		res, err := client.CreateContacts(ctx, proton.CreateContactsReq{
			Contacts:  []proton.ContactCards{{Cards: proton.Cards{clearCard}}},
			Overwrite: 1,
		})
		if err != nil {
			return fmt.Errorf("contact %s: %w", c.Email, err)
		}

		if len(res) != 1 || res[0].Response.Code != proton.SuccessCode {
			return fmt.Errorf("contact %s: %+v", c.Email, res)
		}

		if _, err := client.UpdateContact(ctx, res[0].Response.Contact.ID, proton.UpdateContactReq{
			Cards: proton.Cards{signed},
		}); err != nil {
			return fmt.Errorf("contact %s: %w", c.Email, err)
		}
	}

	return nil
}
